//! Applies versioned seed bundles (spec section 4.6) to the database.
//!
//! Each version is applied once, in one transaction, and recorded in
//! `seed_version`. It only inserts. A version that would add an object that
//! already exists rolls back entirely and is reported as
//! [`SeedError::Conflict`], naming the object.

use std::collections::HashMap;

use configurator_core::axis::Cardinality;
use configurator_core::canonical::sha256_hex;
use configurator_core::layer::{LayerKey, Scope};
use configurator_core::overlay::{Kind, Op};
use configurator_core::seed::{self, SeedBundle};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Advisory-lock key that serialises seeding across replicas starting at once.
const SEED_LOCK: i64 = 0x5eed_c0f1;

const V1: &[(&str, &str)] = &[
    ("axes.toml", include_str!("../../../seed/v1/axes.toml")),
    (
        "dimensions.toml",
        include_str!("../../../seed/v1/dimensions.toml"),
    ),
    ("base.toml", include_str!("../../../seed/v1/base.toml")),
    ("retail.toml", include_str!("../../../seed/v1/retail.toml")),
    (
        "electricity.toml",
        include_str!("../../../seed/v1/electricity.toml"),
    ),
    ("jobs.toml", include_str!("../../../seed/v1/jobs.toml")),
];

/// Every seed version shipped in this build, as `(version, files)`.
const EMBEDDED: &[(u32, &[(&str, &str)])] = &[(1, V1)];

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("seed v{version} does not parse: {error}")]
    Parse {
        version: u32,
        #[source]
        error: seed::SeedError,
    },
    /// The version would add `key` (`<kind>:<key>`, e.g. `entity:Order`), which already exists.
    #[error("seed v{version} would add {key}, which already exists")]
    Conflict { version: u32, key: String },
    #[error("seed v{version}: database error")]
    Db {
        version: u32,
        #[source]
        error: sqlx::Error,
    },
}

/// The seed bundles embedded in this build, parsed and validated.
pub fn embedded() -> Result<Vec<SeedBundle>, SeedError> {
    EMBEDDED
        .iter()
        .map(|&(version, files)| {
            seed::parse(version, files).map_err(|error| SeedError::Parse { version, error })
        })
        .collect()
}

/// sha256 hex of a bundle's canonical files (`render`), sorted by file name and
/// concatenated as `name \n text \n`. Comments and formatting do not change it.
pub fn bundle_sha256(b: &SeedBundle) -> String {
    let mut files = seed::render(b);
    files.sort_by(|x, y| x.0.cmp(&y.0));
    let mut all = String::new();
    for (name, text) in &files {
        all.push_str(name);
        all.push('\n');
        all.push_str(text);
        all.push('\n');
    }
    sha256_hex(&all)
}

/// The author of every row a seed version inserts.
pub fn author(version: u32) -> String {
    format!("seed:v{version}")
}

/// Apply every bundle not yet recorded in `seed_version`, lowest version first.
/// Returns the versions applied by this call.
pub async fn apply(pool: &PgPool, bundles: &[SeedBundle]) -> Result<Vec<u32>, SeedError> {
    let mut ordered: Vec<&SeedBundle> = bundles.iter().collect();
    ordered.sort_by_key(|b| b.version);
    let mut applied = Vec::new();
    for b in ordered {
        let version = b.version;
        let db = move |error: sqlx::Error| SeedError::Db { version, error };
        let sha = bundle_sha256(b);
        let mut tx = pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SEED_LOCK)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let recorded: Option<String> =
            sqlx::query_scalar("SELECT sha256 FROM seed_version WHERE version = $1")
                .bind(version as i32)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        if let Some(recorded) = recorded {
            if recorded != sha {
                tracing::warn!(
                    version,
                    recorded = %recorded,
                    embedded = %sha,
                    "seed version already applied with different content; not re-applied"
                );
            }
            tx.rollback().await.map_err(db)?;
            continue;
        }
        insert_bundle(&mut tx, b).await?;
        sqlx::query("INSERT INTO seed_version (version, sha256) VALUES ($1, $2)")
            .bind(version as i32)
            .bind(sha.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        tracing::info!(version, "seed version applied");
        applied.push(version);
    }
    Ok(applied)
}

/// A unique violation becomes a `Conflict` naming `key`; anything else is a `Db` error.
fn fail(version: u32, key: String) -> impl FnOnce(sqlx::Error) -> SeedError {
    move |error| {
        let unique = matches!(&error, sqlx::Error::Database(d) if d.is_unique_violation());
        if unique {
            SeedError::Conflict { version, key }
        } else {
            SeedError::Db { version, error }
        }
    }
}

async fn insert_bundle(conn: &mut PgConnection, b: &SeedBundle) -> Result<(), SeedError> {
    let version = b.version;
    let db = move |error: sqlx::Error| SeedError::Db { version, error };
    let author = author(version);

    for a in &b.axes {
        sqlx::query("INSERT INTO axis (key, cardinality) VALUES ($1, $2)")
            .bind(a.key.as_str())
            .bind(cardinality_name(a.cardinality))
            .execute(&mut *conn)
            .await
            .map_err(fail(version, format!("axis:{}", a.key)))?;
        for v in &a.values {
            let label = v.key.rsplit('.').next().unwrap_or(v.key.as_str());
            sqlx::query(
                "INSERT INTO axis_value (axis, key, label, parent, description) \
                 VALUES ($1, $2, $3, $4, NULL)",
            )
            .bind(a.key.as_str())
            .bind(v.key.as_str())
            .bind(label)
            .bind(v.parent.as_deref())
            .execute(&mut *conn)
            .await
            .map_err(fail(version, format!("axis_value:{}={}", a.key, v.key)))?;
        }
    }

    for d in &b.dimensions {
        sqlx::query("INSERT INTO dimension (key, label) VALUES ($1, $2)")
            .bind(d.key.as_str())
            .bind(d.label.as_str())
            .execute(&mut *conn)
            .await
            .map_err(fail(version, format!("dimension:{}", d.key)))?;
        for (key, label) in &d.values {
            sqlx::query(
                "INSERT INTO dimension_value (dimension, key, label, tenant) \
                 VALUES ($1, $2, $3, NULL)",
            )
            .bind(d.key.as_str())
            .bind(key.as_str())
            .bind(label.as_str())
            .execute(&mut *conn)
            .await
            .map_err(fail(version, format!("dimension_value:{}={key}", d.key)))?;
        }
    }

    let mut layers: HashMap<String, Uuid> = HashMap::new();

    for r in &b.rules {
        let layer = seed::rule_layer(&r.rule.key);
        let layer_id = ensure_layer(conn, &mut layers, &layer).await.map_err(db)?;
        let condition =
            serde_json::to_value(&r.rule.condition).expect("a condition serializes to JSON");
        sqlx::query(
            "INSERT INTO axis_rule (key, priority, condition, layer_id) VALUES ($1, $2, $3, $4)",
        )
        .bind(r.rule.key.as_str())
        .bind(r.rule.priority)
        .bind(condition)
        .bind(layer_id)
        .execute(&mut *conn)
        .await
        .map_err(fail(version, format!("rule:{}", r.rule.key)))?;
    }

    for p in &b.profiles {
        let values = serde_json::to_value(&p.values).expect("a profile serializes to JSON");
        sqlx::query(r#"INSERT INTO profile (tenant, name, "values") VALUES (NULL, $1, $2)"#)
            .bind(p.name.as_str())
            .bind(values)
            .execute(&mut *conn)
            .await
            .map_err(fail(version, format!("profile:{}", p.name)))?;
    }

    for l in &b.layers {
        ensure_layer(conn, &mut layers, &l.layer)
            .await
            .map_err(db)?;
    }

    let (_, rows) = seed::to_rows(b);
    for row in &rows {
        let layer_id = ensure_layer(conn, &mut layers, &row.layer)
            .await
            .map_err(db)?;
        sqlx::query(
            "INSERT INTO overlay_row (id, layer_id, kind, key, op, fields, rev, is_current, author) \
             VALUES ($1, $2, $3, $4, $5, $6, 1, true, $7)",
        )
        .bind(row.id)
        .bind(layer_id)
        .bind(kind_name(row.kind))
        .bind(row.key.as_str())
        .bind(op_name(row.op))
        .bind(Value::Object(row.fields.clone()))
        .bind(author.as_str())
        .execute(&mut *conn)
        .await
        .map_err(fail(version, format!("{}:{}", kind_name(row.kind), row.key)))?;
    }
    Ok(())
}

/// The id of the layer row for `l`, inserting it when it does not exist yet.
/// Layers are shared: a seed reuses an existing layer, it never conflicts on one.
async fn ensure_layer(
    conn: &mut PgConnection,
    cache: &mut HashMap<String, Uuid>,
    l: &LayerKey,
) -> Result<Uuid, sqlx::Error> {
    let path = seed::layer_path(l);
    if let Some(id) = cache.get(&path) {
        return Ok(*id);
    }
    let (dim_key, dim_value) = match &l.dimension {
        Some((k, v)) => (Some(k.as_str()), Some(v.as_str())),
        None => (None, None),
    };
    sqlx::query(
        "INSERT INTO layer (scope, industry, department, job, rule, dimension_key, dimension_value, tenant) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT ON CONSTRAINT layer_key_unique DO NOTHING",
    )
    .bind(scope_name(l.scope))
    .bind(l.industry.as_deref())
    .bind(l.department.as_deref())
    .bind(l.job.as_deref())
    .bind(l.rule.as_deref())
    .bind(dim_key)
    .bind(dim_value)
    .bind(l.tenant)
    .execute(&mut *conn)
    .await?;
    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM layer WHERE scope = $1 \
         AND industry IS NOT DISTINCT FROM $2 \
         AND department IS NOT DISTINCT FROM $3 \
         AND job IS NOT DISTINCT FROM $4 \
         AND rule IS NOT DISTINCT FROM $5 \
         AND dimension_key IS NOT DISTINCT FROM $6 \
         AND dimension_value IS NOT DISTINCT FROM $7 \
         AND tenant IS NOT DISTINCT FROM $8",
    )
    .bind(scope_name(l.scope))
    .bind(l.industry.as_deref())
    .bind(l.department.as_deref())
    .bind(l.job.as_deref())
    .bind(l.rule.as_deref())
    .bind(dim_key)
    .bind(dim_value)
    .bind(l.tenant)
    .fetch_one(&mut *conn)
    .await?;
    cache.insert(path, id);
    Ok(id)
}

// --- column values ----------------------------------------------------------------

pub fn scope_name(s: Scope) -> &'static str {
    match s {
        Scope::Base => "base",
        Scope::Profile => "profile",
        Scope::Industry => "industry",
        Scope::IndustryDept => "industry_dept",
        Scope::Job => "job",
        Scope::Tenant => "tenant",
    }
}

pub fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Entity => "entity",
        Kind::Attribute => "attribute",
        Kind::Relation => "relation",
        Kind::Job => "job",
    }
}

pub fn op_name(op: Op) -> &'static str {
    match op {
        Op::Add => "add",
        Op::Patch => "patch",
        Op::Tombstone => "tombstone",
    }
}

fn cardinality_name(c: Cardinality) -> &'static str {
    match c {
        Cardinality::One => "one",
        Cardinality::Many => "many",
    }
}
