//! `tenant_license`: the licence issued to each tenant and its token, sealed (see
//! [`crate::license_token`]; this module never sees the token in clear). Owner-only (the runtime role
//! has no grant): every query here takes the owner pool or an owner transaction.
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::licenses_admin::Issued;

/// The sealed licence token of `tenant`; `None` when it holds none.
pub async fn sealed_token(owner: &PgPool, tenant: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT token_encrypted FROM tenant_license WHERE tenant = $1")
        .bind(tenant)
        .fetch_optional(owner)
        .await
}

/// The licence id of `tenant`; `None` when it holds none.
pub async fn license_id(owner: &PgPool, tenant: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT license_id FROM tenant_license WHERE tenant = $1")
        .bind(tenant)
        .fetch_optional(owner)
        .await
}

/// Store the licence just issued to `tenant`, held by `holder_sub`; `token_encrypted` is
/// `issued.token` sealed for `tenant`.
pub async fn store(
    tx: &mut PgConnection,
    tenant: Uuid,
    issued: &Issued,
    holder_sub: &str,
    token_encrypted: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO tenant_license (tenant, license_id, subject_id, holder_sub, token_encrypted) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tenant)
    .bind(issued.license_id.as_str())
    .bind(issued.subject_id.as_str())
    .bind(holder_sub)
    .bind(token_encrypted)
    .execute(tx)
    .await?;
    Ok(())
}

/// Forget licence `license_id` of `tenant` (after it was revoked upstream). Keyed on both, so a
/// licence issued again meanwhile is not forgotten with it.
pub async fn remove(
    tx: &mut PgConnection,
    tenant: Uuid,
    license_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM tenant_license WHERE tenant = $1 AND license_id = $2")
        .bind(tenant)
        .bind(license_id)
        .execute(tx)
        .await?;
    Ok(())
}
