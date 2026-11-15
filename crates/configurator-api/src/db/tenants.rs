//! `tenant` queries. Writes take the caller's transaction so the audit event shares it. `tenant` is
//! outside RLS: identity and tenancy are resolved before a tenant is known.
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::users;
use crate::page::TimeCursor;
use crate::tenancy::Role;

#[derive(Debug, Clone, Serialize)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339; null while the tenant is live, set once when it is offboarded.
    pub archived_at: Option<String>,
    /// `created_at` in microseconds since the epoch: the paging cursor, not part of the body.
    #[serde(skip)]
    pub ts_us: i64,
}

/// id, name, created_at, archived_at, ts_us
type TenantTuple = (Uuid, String, String, Option<String>, i64);

// `to_jsonb(timestamptz) #>> '{}'` is RFC 3339 text (the crate carries no date-time type; NULL stays
// NULL); the microsecond epoch is the exact paging position.
const COLUMNS: &str = "id, name, to_jsonb(created_at) #>> '{}', to_jsonb(archived_at) #>> '{}', \
                       (extract(epoch FROM created_at) * 1000000)::bigint";

fn tenant((id, name, created_at, archived_at, ts_us): TenantTuple) -> Tenant {
    Tenant {
        id,
        name,
        created_at,
        archived_at,
        ts_us,
    }
}

fn tenants(rows: Vec<TenantTuple>) -> Vec<Tenant> {
    rows.into_iter().map(tenant).collect()
}

// Lists are newest first; `$1`/`$2` are the `before` cursor (both NULL on the first page).

/// Every tenant (super-admin view), one page; archived tenants only with `include_archived`.
pub async fn list_all(
    db: &PgPool,
    before: Option<TimeCursor>,
    fetch: i64,
    include_archived: bool,
) -> Result<Vec<Tenant>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, TenantTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM tenant \
         WHERE ($4::boolean OR archived_at IS NULL) \
           AND ($1::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $1::bigint * interval '1 microsecond', $2::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $3"
    )))
    .bind(us)
    .bind(id)
    .bind(fetch)
    .bind(include_archived)
    .fetch_all(db)
    .await?;
    Ok(tenants(rows))
}

/// The tenant `sub` belongs to (an active `users` row), as a page of zero or one; never an archived
/// one.
pub async fn list_for(
    db: &PgPool,
    sub: &str,
    before: Option<TimeCursor>,
    fetch: i64,
) -> Result<Vec<Tenant>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, TenantTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM tenant \
         WHERE id IN (SELECT tenant FROM users WHERE sub = $4 AND disabled_at IS NULL) \
           AND archived_at IS NULL \
           AND ($1::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $1::bigint * interval '1 microsecond', $2::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $3"
    )))
    .bind(us)
    .bind(id)
    .bind(fetch)
    .bind(sub)
    .fetch_all(db)
    .await?;
    Ok(tenants(rows))
}

/// Insert the tenant `id` (minted by the caller with `Uuid::new_v4`: the configurator defines tenant
/// ids, and the licence is issued for the id before it is inserted) and its first `tenant_admin`.
/// `None` when `first_admin_sub` already belongs to a tenant (one tenant per identity); the caller then
/// drops the transaction, so the tenant is not created either.
pub async fn create(
    tx: &mut PgConnection,
    id: Uuid,
    name: &str,
    first_admin_sub: &str,
) -> Result<Option<Tenant>, sqlx::Error> {
    let row = sqlx::query_as::<_, TenantTuple>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO tenant (id, name) VALUES ($1, $2) RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    if users::add(&mut *tx, id, first_admin_sub, Role::TenantAdmin)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(tenant(row)))
}

/// Whether the tenant is archived; `None` when it does not exist.
pub async fn archived(db: &PgPool, id: Uuid) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT archived_at IS NOT NULL FROM tenant WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The tenant `id`, archived or not; `None` when it does not exist.
pub async fn get(db: &PgPool, id: Uuid) -> Result<Option<Tenant>, sqlx::Error> {
    let row = sqlx::query_as::<_, TenantTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM tenant WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(tenant))
}

/// The tenant `id`, locked for the rest of the caller's transaction; `None` when it does not exist.
/// Two concurrent offboards of one tenant serialise here.
pub async fn lock(tx: &mut PgConnection, id: Uuid) -> Result<Option<Tenant>, sqlx::Error> {
    let row = sqlx::query_as::<_, TenantTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM tenant WHERE id = $1 FOR UPDATE"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row.map(tenant))
}

/// Archive the (locked) tenant `id`. Write-once: an archived tenant keeps its original `archived_at`.
/// Returns the row as it now is.
pub async fn archive(tx: &mut PgConnection, id: Uuid) -> Result<Tenant, sqlx::Error> {
    let row = sqlx::query_as::<_, TenantTuple>(sqlx::AssertSqlSafe(format!(
        "UPDATE tenant SET archived_at = COALESCE(archived_at, now()) WHERE id = $1 \
         RETURNING {COLUMNS}"
    )))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(tenant(row))
}
