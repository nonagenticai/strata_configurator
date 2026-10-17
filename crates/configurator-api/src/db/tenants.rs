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
    /// `created_at` in microseconds since the epoch: the paging cursor, not part of the body.
    #[serde(skip)]
    pub ts_us: i64,
}

fn tenants(rows: Vec<(Uuid, String, String, i64)>) -> Vec<Tenant> {
    rows.into_iter()
        .map(|(id, name, created_at, ts_us)| Tenant {
            id,
            name,
            created_at,
            ts_us,
        })
        .collect()
}

// `to_jsonb(timestamptz) #>> '{}'` is RFC 3339 text (the crate carries no date-time type); the
// microsecond epoch is the exact paging position. Lists are newest first; `$1`/`$2` are the `before`
// cursor (both NULL on the first page).

/// Every tenant (super-admin view), one page.
pub async fn list_all(
    db: &PgPool,
    before: Option<TimeCursor>,
    fetch: i64,
) -> Result<Vec<Tenant>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, (Uuid, String, String, i64)>(
        "SELECT id, name, to_jsonb(created_at) #>> '{}', \
                (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM tenant \
         WHERE ($1::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $1::bigint * interval '1 microsecond', $2::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $3",
    )
    .bind(us)
    .bind(id)
    .bind(fetch)
    .fetch_all(db)
    .await?;
    Ok(tenants(rows))
}

/// The tenant `sub` belongs to (an active `users` row), as a page of zero or one.
pub async fn list_for(
    db: &PgPool,
    sub: &str,
    before: Option<TimeCursor>,
    fetch: i64,
) -> Result<Vec<Tenant>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, (Uuid, String, String, i64)>(
        "SELECT id, name, to_jsonb(created_at) #>> '{}', \
                (extract(epoch FROM created_at) * 1000000)::bigint \
         FROM tenant \
         WHERE id IN (SELECT tenant FROM users WHERE sub = $4 AND disabled_at IS NULL) \
           AND ($1::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $1::bigint * interval '1 microsecond', $2::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $3",
    )
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
    let (id, name, created_at, ts_us) = sqlx::query_as::<_, (Uuid, String, String, i64)>(
        "INSERT INTO tenant (id, name) VALUES ($1, $2) \
         RETURNING id, name, to_jsonb(created_at) #>> '{}', \
                   (extract(epoch FROM created_at) * 1000000)::bigint",
    )
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
    Ok(Some(Tenant {
        id,
        name,
        created_at,
        ts_us,
    }))
}

/// `true` when the tenant exists.
pub async fn exists(db: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM tenant WHERE id = $1)")
        .bind(id)
        .fetch_one(db)
        .await
}
