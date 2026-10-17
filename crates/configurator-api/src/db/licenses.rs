//! `tenant_license`: the licence issued to each tenant and its token. Owner-only (the runtime role has
//! no grant): every query here takes the owner pool or an owner transaction.
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::licenses_admin::Issued;

/// The licence token of `tenant`; `None` when it holds none.
pub async fn token(owner: &PgPool, tenant: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT token FROM tenant_license WHERE tenant = $1")
        .bind(tenant)
        .fetch_optional(owner)
        .await
}

/// Store the licence just issued to `tenant`, held by `holder_sub`.
pub async fn store(
    tx: &mut PgConnection,
    tenant: Uuid,
    issued: &Issued,
    holder_sub: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO tenant_license (tenant, license_id, subject_id, holder_sub, token) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tenant)
    .bind(issued.license_id.as_str())
    .bind(issued.subject_id.as_str())
    .bind(holder_sub)
    .bind(issued.token.as_str())
    .execute(tx)
    .await?;
    Ok(())
}
