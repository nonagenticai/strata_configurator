//! `users`: who belongs to which tenant, with which tenant role (commonpractice's identity model). One
//! row per identity (`sub` is the primary key), so one tenant per identity. A disabled user keeps the
//! row (and so the identity stays bound to that tenant) but resolves to no workspace. `users` is
//! outside RLS: it is read to find the tenant before any tenant is set. Writes take the caller's
//! transaction so the audit event shares it.
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tenancy::Role;

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub sub: String,
    pub role: Role,
    /// RFC 3339; null while the user is active.
    pub disabled_at: Option<String>,
    /// RFC 3339.
    pub created_at: String,
}

/// The caller's own workspace, for `GET /api/me`.
#[derive(Debug, Clone, Serialize)]
pub struct Home {
    pub id: Uuid,
    pub name: String,
    pub role: Role,
}

fn role(s: &str) -> Result<Role, sqlx::Error> {
    Role::parse(s).ok_or_else(|| sqlx::Error::Decode(format!("unknown tenant_role {s:?}").into()))
}

/// sub, tenant_role, disabled_at, created_at
type UserTuple = (String, String, Option<String>, String);

const COLUMNS: &str = "sub, tenant_role::text, to_jsonb(disabled_at) #>> '{}', \
                       to_jsonb(created_at) #>> '{}'";

fn user((sub, r, disabled_at, created_at): UserTuple) -> Result<User, sqlx::Error> {
    Ok(User {
        sub,
        role: role(&r)?,
        disabled_at,
        created_at,
    })
}

fn users(rows: Vec<UserTuple>) -> Result<Vec<User>, sqlx::Error> {
    rows.into_iter().map(user).collect()
}

/// The tenant and role of `sub`'s active row; `None` when there is no row or it is disabled.
pub async fn home(db: &PgPool, sub: &str) -> Result<Option<(Uuid, Role)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT tenant, tenant_role::text FROM users WHERE sub = $1 AND disabled_at IS NULL",
    )
    .bind(sub)
    .fetch_optional(db)
    .await?;
    row.map(|(t, r)| -> Result<(Uuid, Role), sqlx::Error> { Ok((t, role(&r)?)) })
        .transpose()
}

/// `true` when `sub` has a row, active or disabled, in any tenant (so it cannot be added anywhere).
pub async fn exists(db: &PgPool, sub: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM users WHERE sub = $1)")
        .bind(sub)
        .fetch_one(db)
        .await
}

/// `home` with the tenant's name.
pub async fn home_tenant(db: &PgPool, sub: &str) -> Result<Option<Home>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT t.id, t.name, u.tenant_role::text FROM users u JOIN tenant t ON t.id = u.tenant \
         WHERE u.sub = $1 AND u.disabled_at IS NULL",
    )
    .bind(sub)
    .fetch_optional(db)
    .await?;
    row.map(|(id, name, r)| -> Result<Home, sqlx::Error> {
        Ok(Home {
            id,
            name,
            role: role(&r)?,
        })
    })
    .transpose()
}

/// Every user of `tenant`, active and disabled, by `sub`.
pub async fn list(db: &PgPool, tenant: Uuid) -> Result<Vec<User>, sqlx::Error> {
    let rows = sqlx::query_as::<_, UserTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM users WHERE tenant = $1 ORDER BY sub"
    )))
    .bind(tenant)
    .fetch_all(db)
    .await?;
    users(rows)
}

/// Add `sub` to `tenant`; `None` when `sub` already has a row in any tenant (nothing is written).
pub async fn add(
    tx: &mut PgConnection,
    tenant: Uuid,
    sub: &str,
    r: Role,
) -> Result<Option<User>, sqlx::Error> {
    let rows = sqlx::query_as::<_, UserTuple>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO users (sub, tenant, tenant_role) VALUES ($1, $2, $3::tenant_role) \
         ON CONFLICT (sub) DO NOTHING RETURNING {COLUMNS}"
    )))
    .bind(sub)
    .bind(tenant)
    .bind(r.as_str())
    .fetch_all(&mut *tx)
    .await?;
    Ok(users(rows)?.into_iter().next())
}

/// Lock the tenant's active `tenant_admin` rows and return their subjects. Two concurrent demotions or
/// disables of different admins serialise here, so the second sees the first's change and cannot
/// remove the last admin.
pub async fn lock_admins(tx: &mut PgConnection, tenant: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT sub FROM users \
         WHERE tenant = $1 AND tenant_role = 'tenant_admin' AND disabled_at IS NULL \
         ORDER BY sub FOR UPDATE",
    )
    .bind(tenant)
    .fetch_all(&mut *tx)
    .await
}

/// The active user `sub` of `tenant`, locked; `None` when there is none (or they are disabled).
pub async fn lock_active(
    tx: &mut PgConnection,
    tenant: Uuid,
    sub: &str,
) -> Result<Option<User>, sqlx::Error> {
    let rows = sqlx::query_as::<_, UserTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM users \
         WHERE tenant = $1 AND sub = $2 AND disabled_at IS NULL FOR UPDATE"
    )))
    .bind(tenant)
    .bind(sub)
    .fetch_all(&mut *tx)
    .await?;
    Ok(users(rows)?.into_iter().next())
}

/// Set the role of the (locked, active) user `sub`; returns the row as it now is.
pub async fn set_role(
    tx: &mut PgConnection,
    tenant: Uuid,
    sub: &str,
    r: Role,
) -> Result<User, sqlx::Error> {
    let row = sqlx::query_as::<_, UserTuple>(sqlx::AssertSqlSafe(format!(
        "UPDATE users SET tenant_role = $3::tenant_role WHERE tenant = $1 AND sub = $2 \
         RETURNING {COLUMNS}"
    )))
    .bind(tenant)
    .bind(sub)
    .bind(r.as_str())
    .fetch_one(&mut *tx)
    .await?;
    user(row)
}

/// Disable the (locked, active) user `sub`; returns the row as it now is.
pub async fn disable(tx: &mut PgConnection, tenant: Uuid, sub: &str) -> Result<User, sqlx::Error> {
    let row = sqlx::query_as::<_, UserTuple>(sqlx::AssertSqlSafe(format!(
        "UPDATE users SET disabled_at = now() WHERE tenant = $1 AND sub = $2 RETURNING {COLUMNS}"
    )))
    .bind(tenant)
    .bind(sub)
    .fetch_one(&mut *tx)
    .await?;
    user(row)
}

/// Disable every active user of `tenant` (its offboarding); disabled users keep their original
/// `disabled_at`. The rows stay: users are never deleted. Returns how many this call disabled.
pub async fn disable_all(tx: &mut PgConnection, tenant: Uuid) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE users SET disabled_at = now() WHERE tenant = $1 AND disabled_at IS NULL",
    )
    .bind(tenant)
    .execute(tx)
    .await?;
    Ok(r.rows_affected())
}
