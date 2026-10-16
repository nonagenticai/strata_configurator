//! `overlay_row` queries (spec section 4.1). A layer holds at most one current row per `(kind, key)`; an
//! edit inserts a new revision and retires the previous one, and a retire keeps the row as history.
//! Writes take the caller's transaction so the audit event shares it.
use configurator_core::layer::LayerKey;
use configurator_core::overlay::{Kind, Op, OverlayRow};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::from_text;
use crate::seeder::{kind_name, op_name, scope_name};

/// One revision of one layered object.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: Uuid,
    pub layer_id: Uuid,
    pub kind: Kind,
    pub key: String,
    pub op: Op,
    pub fields: Map<String, Value>,
    pub rev: i32,
    pub is_current: bool,
    /// The `sub` of whoever wrote this revision.
    pub author: String,
    /// RFC 3339.
    pub created_at: String,
    /// `created_at` in microseconds since the epoch: the paging cursor, not part of the body.
    #[serde(skip)]
    pub ts_us: i64,
}

/// id, layer_id, kind, key, op, fields, rev, is_current, author, created_at, ts_us
type RowTuple = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    Value,
    i32,
    bool,
    String,
    String,
    i64,
);

fn object(v: Value) -> Result<Map<String, Value>, sqlx::Error> {
    match v {
        Value::Object(m) => Ok(m),
        other => Err(sqlx::Error::Decode(
            format!("overlay_row.fields is not an object: {other}").into(),
        )),
    }
}

fn row(t: RowTuple) -> Result<Row, sqlx::Error> {
    let (id, layer_id, kind, key, op, fields, rev, is_current, author, created_at, ts_us) = t;
    Ok(Row {
        id,
        layer_id,
        kind: from_text(&kind)?,
        key,
        op: from_text(&op)?,
        fields: object(fields)?,
        rev,
        is_current,
        author,
        created_at,
        ts_us,
    })
}

fn rows(ts: Vec<RowTuple>) -> Result<Vec<Row>, sqlx::Error> {
    ts.into_iter().map(row).collect()
}

/// `true` for the unique violations two racing writers of one `(layer, kind, key)` can hit: a second
/// current row (`overlay_row_current_unique`) or a duplicate revision number (`overlay_row_rev_unique`).
/// Callers turn these into a 409, never a 500.
pub fn is_conflict(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(d) => {
            d.is_unique_violation()
                && matches!(
                    d.constraint(),
                    Some("overlay_row_current_unique" | "overlay_row_rev_unique")
                )
        }
        _ => false,
    }
}

/// One page of the layer's current rows, optionally of one kind. Newest first.
pub async fn list_current(
    db: &PgPool,
    layer_id: Uuid,
    kind: Option<Kind>,
    before: Option<crate::page::TimeCursor>,
    fetch: i64,
) -> Result<Vec<Row>, sqlx::Error> {
    let (us, id) = crate::page::TimeCursor::split(before);
    let ts = sqlx::query_as::<_, RowTuple>(
        "SELECT id, layer_id, kind, key, op, fields, rev, is_current, author, \
                to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM overlay_row \
         WHERE layer_id = $1 AND is_current \
           AND ($2::text IS NULL OR kind = $2::text) \
           AND ($3::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $3::bigint * interval '1 microsecond', $4::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $5",
    )
    .bind(layer_id)
    .bind(kind.map(kind_name))
    .bind(us)
    .bind(id)
    .bind(fetch)
    .fetch_all(db)
    .await?;
    rows(ts)
}

/// The newest revision of `(layer, kind, key)`, current or retired; `None` when it never existed.
/// The current revision, when there is one, is always the newest.
pub async fn latest(
    tx: &mut PgConnection,
    layer_id: Uuid,
    kind: Kind,
    key: &str,
) -> Result<Option<Row>, sqlx::Error> {
    let ts = sqlx::query_as::<_, RowTuple>(
        "SELECT id, layer_id, kind, key, op, fields, rev, is_current, author, \
                to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM overlay_row WHERE layer_id = $1 AND kind = $2 AND key = $3 \
         ORDER BY rev DESC LIMIT 1",
    )
    .bind(layer_id)
    .bind(kind_name(kind))
    .bind(key)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows(ts)?.into_iter().next())
}

/// A revision to insert as the current one.
#[derive(Debug, Clone, Copy)]
pub struct NewRev<'a> {
    pub layer_id: Uuid,
    pub kind: Kind,
    pub key: &'a str,
    pub op: Op,
    pub fields: &'a Map<String, Value>,
    pub rev: i32,
    pub author: &'a str,
}

/// Insert `r` as the current revision. A racing writer surfaces as an error [`is_conflict`] accepts.
pub async fn insert(tx: &mut PgConnection, r: NewRev<'_>) -> Result<Row, sqlx::Error> {
    let t = sqlx::query_as::<_, RowTuple>(
        "INSERT INTO overlay_row (id, layer_id, kind, key, op, fields, rev, is_current, author) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, true, $8) \
         RETURNING id, layer_id, kind, key, op, fields, rev, is_current, author, \
                   to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint",
    )
    .bind(Uuid::new_v4())
    .bind(r.layer_id)
    .bind(kind_name(r.kind))
    .bind(r.key)
    .bind(op_name(r.op))
    .bind(Value::Object(r.fields.clone()))
    .bind(r.rev)
    .bind(r.author)
    .fetch_one(&mut *tx)
    .await?;
    row(t)
}

/// Retire the current revision of `(layer, kind, key)` when it is `expected_rev`, returning it as it was
/// (still `is_current`). `None` when there is no current revision or it is another one. Of two
/// concurrent callers with the same `expected_rev`, the second waits on the first's row lock and then
/// finds the row no longer current, so it gets `None`.
pub async fn retire(
    tx: &mut PgConnection,
    layer_id: Uuid,
    kind: Kind,
    key: &str,
    expected_rev: i32,
) -> Result<Option<Row>, sqlx::Error> {
    let ts = sqlx::query_as::<_, RowTuple>(
        "UPDATE overlay_row SET is_current = false \
         WHERE layer_id = $1 AND kind = $2 AND key = $3 AND is_current AND rev = $4 \
         RETURNING id, layer_id, kind, key, op, fields, rev, true, author, \
                   to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint",
    )
    .bind(layer_id)
    .bind(kind_name(kind))
    .bind(key)
    .bind(expected_rev)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows(ts)?.into_iter().next())
}

/// One page of every revision of `(layer, kind, key)`, newest first; `before` is a revision number.
pub async fn revisions(
    db: &PgPool,
    layer_id: Uuid,
    kind: Kind,
    key: &str,
    before: Option<i32>,
    fetch: i64,
) -> Result<Vec<Row>, sqlx::Error> {
    let ts = sqlx::query_as::<_, RowTuple>(
        "SELECT id, layer_id, kind, key, op, fields, rev, is_current, author, \
                to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM overlay_row WHERE layer_id = $1 AND kind = $2 AND key = $3 \
           AND ($4::integer IS NULL OR rev < $4::integer) \
         ORDER BY rev DESC LIMIT $5",
    )
    .bind(layer_id)
    .bind(kind_name(kind))
    .bind(key)
    .bind(before)
    .bind(fetch)
    .fetch_all(db)
    .await?;
    rows(ts)
}

/// The current rows of exactly `layers` (each matched on every key column), in `layers` order and by
/// `(kind, key)` within a layer. A layer that does not exist yields no rows.
pub async fn load_for_layers(
    pool: &PgPool,
    layers: &[LayerKey],
) -> Result<Vec<OverlayRow>, sqlx::Error> {
    let mut out = Vec::new();
    for l in layers {
        let (dim_key, dim_value) = match &l.dimension {
            Some((k, v)) => (Some(k.as_str()), Some(v.as_str())),
            None => (None, None),
        };
        let ts = sqlx::query_as::<_, (Uuid, String, String, String, Value)>(
            "SELECT r.id, r.kind, r.key, r.op, r.fields \
             FROM overlay_row r JOIN layer l ON l.id = r.layer_id \
             WHERE r.is_current AND l.scope = $1 \
               AND l.industry IS NOT DISTINCT FROM $2::text \
               AND l.department IS NOT DISTINCT FROM $3::text \
               AND l.job IS NOT DISTINCT FROM $4::text \
               AND l.rule IS NOT DISTINCT FROM $5::text \
               AND l.dimension_key IS NOT DISTINCT FROM $6::text \
               AND l.dimension_value IS NOT DISTINCT FROM $7::text \
               AND l.tenant IS NOT DISTINCT FROM $8::uuid \
             ORDER BY r.kind, r.key",
        )
        .bind(scope_name(l.scope))
        .bind(l.industry.as_deref())
        .bind(l.department.as_deref())
        .bind(l.job.as_deref())
        .bind(l.rule.as_deref())
        .bind(dim_key)
        .bind(dim_value)
        .bind(l.tenant)
        .fetch_all(pool)
        .await?;
        for (id, kind, key, op, fields) in ts {
            out.push(OverlayRow {
                id,
                layer: l.clone(),
                kind: from_text(&kind)?,
                key,
                op: from_text(&op)?,
                fields: object(fields)?,
            });
        }
    }
    Ok(out)
}
