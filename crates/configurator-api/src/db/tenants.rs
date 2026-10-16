//! `tenant` and `member` queries. Writes take the caller's transaction so the audit event shares it.
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tenancy::Role;

#[derive(Debug, Clone, Serialize)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    /// RFC 3339.
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Membership {
    pub tenant: Uuid,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub sub: String,
    pub role: Role,
    /// RFC 3339.
    pub created_at: String,
}

fn role(s: &str) -> Result<Role, sqlx::Error> {
    Role::parse(s).ok_or_else(|| sqlx::Error::Decode(format!("unknown member_role {s:?}").into()))
}

/// `None` when the tenant does not exist; `Some(None)` when it exists and `sub` is not a member.
pub async fn role_in(
    db: &PgPool,
    tenant: Uuid,
    sub: &str,
) -> Result<Option<Option<Role>>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Option<String>,)>(
        "SELECT m.role::text FROM tenant t \
         LEFT JOIN member m ON m.tenant = t.id AND m.sub = $2 \
         WHERE t.id = $1",
    )
    .bind(tenant)
    .bind(sub)
    .fetch_optional(db)
    .await?;
    match row {
        None => Ok(None),
        Some((None,)) => Ok(Some(None)),
        Some((Some(r),)) => Ok(Some(Some(role(&r)?))),
    }
}

/// Every tenant `sub` belongs to, by name.
pub async fn memberships(db: &PgPool, sub: &str) -> Result<Vec<Membership>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT t.id, t.name, m.role::text FROM member m \
         JOIN tenant t ON t.id = m.tenant \
         WHERE m.sub = $1 ORDER BY t.name, t.id",
    )
    .bind(sub)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(tenant, name, r)| -> Result<Membership, sqlx::Error> {
            Ok(Membership {
                tenant,
                name,
                role: role(&r)?,
            })
        })
        .collect()
}

fn tenants(rows: Vec<(Uuid, String, String)>) -> Vec<Tenant> {
    rows.into_iter()
        .map(|(id, name, created_at)| Tenant {
            id,
            name,
            created_at,
        })
        .collect()
}

/// Every tenant (super-admin view).
pub async fn list_all(db: &PgPool) -> Result<Vec<Tenant>, sqlx::Error> {
    // `to_jsonb(timestamptz) #>> '{}'` is RFC 3339 text (the crate carries no date-time type).
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, name, to_jsonb(created_at) #>> '{}' FROM tenant ORDER BY created_at, id",
    )
    .fetch_all(db)
    .await?;
    Ok(tenants(rows))
}

/// The tenants `sub` is a member of.
pub async fn list_for(db: &PgPool, sub: &str) -> Result<Vec<Tenant>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, name, to_jsonb(created_at) #>> '{}' FROM tenant \
         WHERE id IN (SELECT tenant FROM member WHERE sub = $1) ORDER BY created_at, id",
    )
    .bind(sub)
    .fetch_all(db)
    .await?;
    Ok(tenants(rows))
}

/// Insert a tenant and its first `admin` member.
pub async fn create(
    tx: &mut PgConnection,
    name: &str,
    first_admin_sub: &str,
) -> Result<Tenant, sqlx::Error> {
    let (id, name, created_at) = sqlx::query_as::<_, (Uuid, String, String)>(
        "INSERT INTO tenant (id, name) VALUES ($1, $2) \
         RETURNING id, name, to_jsonb(created_at) #>> '{}'",
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO member (tenant, sub, role) VALUES ($1, $2, 'admin')")
        .bind(id)
        .bind(first_admin_sub)
        .execute(&mut *tx)
        .await?;
    Ok(Tenant {
        id,
        name,
        created_at,
    })
}

fn members(rows: Vec<(String, String, String)>) -> Result<Vec<Member>, sqlx::Error> {
    rows.into_iter()
        .map(|(sub, r, created_at)| -> Result<Member, sqlx::Error> {
            Ok(Member {
                sub,
                role: role(&r)?,
                created_at,
            })
        })
        .collect()
}

pub async fn list_members(db: &PgPool, tenant: Uuid) -> Result<Vec<Member>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT sub, role::text, to_jsonb(created_at) #>> '{}' FROM member \
         WHERE tenant = $1 ORDER BY sub",
    )
    .bind(tenant)
    .fetch_all(db)
    .await?;
    members(rows)
}

/// `None` when `sub` is already a member (nothing is written).
pub async fn add_member(
    tx: &mut PgConnection,
    tenant: Uuid,
    sub: &str,
    r: Role,
) -> Result<Option<Member>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "INSERT INTO member (tenant, sub, role) VALUES ($1, $2, $3::member_role) \
         ON CONFLICT (tenant, sub) DO NOTHING \
         RETURNING sub, role::text, to_jsonb(created_at) #>> '{}'",
    )
    .bind(tenant)
    .bind(sub)
    .bind(r.as_str())
    .fetch_all(&mut *tx)
    .await?;
    Ok(members(rows)?.into_iter().next())
}

/// Lock the tenant's admin rows and return their subjects. Two concurrent removals of different
/// admins serialise here, so the second sees the first's delete and cannot remove the last admin.
pub async fn lock_admins(tx: &mut PgConnection, tenant: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT sub FROM member WHERE tenant = $1 AND role = 'admin' ORDER BY sub FOR UPDATE",
    )
    .bind(tenant)
    .fetch_all(&mut *tx)
    .await
}

/// The member's row, locked; `None` when `sub` is not a member.
pub async fn lock_member(
    tx: &mut PgConnection,
    tenant: Uuid,
    sub: &str,
) -> Result<Option<Member>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT sub, role::text, to_jsonb(created_at) #>> '{}' FROM member \
         WHERE tenant = $1 AND sub = $2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(sub)
    .fetch_all(&mut *tx)
    .await?;
    Ok(members(rows)?.into_iter().next())
}

pub async fn remove_member(
    tx: &mut PgConnection,
    tenant: Uuid,
    sub: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM member WHERE tenant = $1 AND sub = $2")
        .bind(tenant)
        .bind(sub)
        .execute(&mut *tx)
        .await?;
    Ok(())
}
