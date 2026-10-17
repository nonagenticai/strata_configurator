//! `layer` queries (spec section 4.1). Writes take the caller's transaction so the audit event shares it.
use configurator_core::layer::LayerKey;
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use super::from_text;
use crate::page::TimeCursor;
use crate::seeder::scope_name;

/// A layer as the API shows it: its id, its key (`scope`, `industry`, `department`, `job`, `rule`,
/// `dimension` as `[key, value]`, `tenant`) and when it was created.
#[derive(Debug, Clone, Serialize)]
pub struct Layer {
    pub id: Uuid,
    #[serde(flatten)]
    pub key: LayerKey,
    /// RFC 3339.
    pub created_at: String,
    /// `created_at` in microseconds since the epoch: the paging cursor, not part of the body.
    #[serde(skip)]
    pub ts_us: i64,
}

/// id, scope, industry, department, job, rule, dimension_key, dimension_value, tenant, created_at, ts_us
type LayerTuple = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    String,
    i64,
);

fn layer(t: LayerTuple) -> Result<Layer, sqlx::Error> {
    let (id, scope, industry, department, job, rule, dim_key, dim_value, tenant, created_at, ts_us) =
        t;
    Ok(Layer {
        id,
        key: LayerKey {
            scope: from_text(&scope)?,
            industry,
            department,
            job,
            rule,
            dimension: dim_key.zip(dim_value),
            tenant,
        },
        created_at,
        ts_us,
    })
}

fn layers(rows: Vec<LayerTuple>) -> Result<Vec<Layer>, sqlx::Error> {
    rows.into_iter().map(layer).collect()
}

/// The layer `id`, if the connection may see it (RLS on the runtime pool). Callers still decide
/// visibility.
pub async fn get(db: &mut PgConnection, id: Uuid) -> Result<Option<Layer>, sqlx::Error> {
    let row = sqlx::query_as::<_, LayerTuple>(
        "SELECT id, scope, industry, department, job, rule, dimension_key, dimension_value, tenant, \
                to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM layer WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    row.map(layer).transpose()
}

/// Optional `GET /api/layers` filters (scope as its column text).
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub scope: Option<&'static str>,
    pub industry: Option<String>,
    pub job: Option<String>,
}

/// One page of the layers visible in `tenant`: every platform layer (`tenant IS NULL`) and that tenant's
/// own layer. `tenant = None` is the platform view (platform layers only). Newest first.
pub async fn list(
    db: &mut PgConnection,
    tenant: Option<Uuid>,
    f: &Filter,
    before: Option<TimeCursor>,
    fetch: i64,
) -> Result<Vec<Layer>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, LayerTuple>(
        "SELECT id, scope, industry, department, job, rule, dimension_key, dimension_value, tenant, \
                to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM layer \
         WHERE (tenant IS NULL OR tenant = $1::uuid) \
           AND ($2::text IS NULL OR scope = $2::text) \
           AND ($3::text IS NULL OR industry = $3::text) \
           AND ($4::text IS NULL OR job = $4::text) \
           AND ($5::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $5::bigint * interval '1 microsecond', $6::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $7",
    )
    .bind(tenant)
    .bind(f.scope)
    .bind(f.industry.as_deref())
    .bind(f.job.as_deref())
    .bind(us)
    .bind(id)
    .bind(fetch)
    .fetch_all(db)
    .await?;
    layers(rows)
}

/// Insert the layer `key` (already validated); `None` when a layer with that key exists (nothing is
/// written).
pub async fn create(tx: &mut PgConnection, key: &LayerKey) -> Result<Option<Layer>, sqlx::Error> {
    let (dim_key, dim_value) = match &key.dimension {
        Some((k, v)) => (Some(k.as_str()), Some(v.as_str())),
        None => (None, None),
    };
    let rows = sqlx::query_as::<_, LayerTuple>(
        "INSERT INTO layer (scope, industry, department, job, rule, dimension_key, dimension_value, tenant) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT ON CONSTRAINT layer_key_unique DO NOTHING \
         RETURNING id, scope, industry, department, job, rule, dimension_key, dimension_value, tenant, \
                   to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint",
    )
    .bind(scope_name(key.scope))
    .bind(key.industry.as_deref())
    .bind(key.department.as_deref())
    .bind(key.job.as_deref())
    .bind(key.rule.as_deref())
    .bind(dim_key)
    .bind(dim_value)
    .bind(key.tenant)
    .fetch_all(&mut *tx)
    .await?;
    Ok(layers(rows)?.into_iter().next())
}
