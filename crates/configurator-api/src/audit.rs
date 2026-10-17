//! Audit trail (spec section 10.2): every write records one `audit_event`, in the write's transaction.
use configurator_core::canonical::{canonical_json, sha256_hex};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

/// sha256 of the canonical JSON of `v`.
pub fn hash(v: &Value) -> String {
    sha256_hex(&canonical_json(v))
}

/// Record one audit event. Pass the write's transaction (`&mut *tx`) so the event commits or rolls back
/// with the write. `before`/`after` are stored as canonical-JSON sha256 hashes, never as values.
pub async fn record(
    tx: &mut PgConnection,
    actor: &str,
    tenant: Option<Uuid>,
    action: &str,
    object: &str,
    before: Option<&Value>,
    after: Option<&Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_event (actor, tenant, action, object, before_hash, after_hash) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(actor)
    .bind(tenant)
    .bind(action)
    .bind(object)
    .bind(before.map(hash))
    .bind(after.map(hash))
    .execute(tx)
    .await?;
    Ok(())
}

/// Record that the super-admin acted in `tenant` through `X-Active-Tenant-Id`: one event per request
/// that switches scope, with the request's method and path as metadata.
pub async fn scope_switch(
    conn: &mut PgConnection,
    actor: &str,
    tenant: Uuid,
    method: &str,
    path: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_event (actor, tenant, action, object, metadata) \
         VALUES ($1, $2, 'tenant.scope_switch', $3, $4)",
    )
    .bind(actor)
    .bind(tenant)
    .bind(format!("tenant:{tenant}"))
    .bind(serde_json::json!({"method": method, "path": path}))
    .execute(conn)
    .await?;
    Ok(())
}
