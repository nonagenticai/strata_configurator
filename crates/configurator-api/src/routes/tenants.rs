//! Tenants and their members (spec sections 10.1 and 10.2).
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit;
use crate::auth::{Caller, require_super_admin};
use crate::db::tenants;
use crate::error::ApiError;
use crate::page::{self, Paged, TimeCursor};
use crate::state::AppState;
use crate::tenancy::{self, Role, TenantCtx};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tenants", get(list).post(create))
        .route("/api/tenants/{id}/members", get(members).post(add_member))
        .route("/api/tenants/{id}/members/{sub}", delete(remove_member))
}

/// A whole (unpaged) list in the `Paged{items,next}` envelope: `next` is null.
fn paged<T: serde::Serialize>(items: Vec<T>) -> Json<Value> {
    Json(json!({"items": items, "next": null}))
}

/// `v` trimmed: 400 `invalid_body` when empty, longer than `max` characters, or holding control
/// characters. The trimmed value is what gets stored.
fn clean(field: &'static str, v: &str, max: usize) -> Result<String, ApiError> {
    let t = v.trim();
    let bad = |why: &str| ApiError::BadRequest("invalid_body", format!("{field} {why}"));
    if t.is_empty() {
        return Err(bad("must not be empty"));
    }
    if t.chars().count() > max {
        return Err(bad(&format!("must be at most {max} characters")));
    }
    if t.chars().any(char::is_control) {
        return Err(bad("must not contain control characters"));
    }
    Ok(t.to_string())
}

const MAX_NAME: usize = 200;
const MAX_SUB: usize = 255;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTenant {
    name: String,
    first_admin_sub: String,
}

/// Super-admin only: the tenant and its first admin, in one transaction, audited.
/// Audit events name the caller by token `sub` (stable; matches `member.sub`).
async fn create(
    State(st): State<AppState>,
    caller: Caller,
    body: Result<Json<NewTenant>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_super_admin(&caller)?;
    let Json(b) = body?;
    let name = clean("name", &b.name, MAX_NAME)?;
    let first_admin_sub = clean("first_admin_sub", &b.first_admin_sub, MAX_SUB)?;
    let mut tx = st.db.begin().await?;
    let t = tenants::create(&mut tx, &name, &first_admin_sub).await?;
    let after = json!({"tenant": t, "first_admin_sub": first_admin_sub});
    audit::record(
        &mut tx,
        &caller.sub,
        Some(t.id),
        "tenant.create",
        &format!("tenant:{}", t.id),
        None,
        Some(&after),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(t))))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<String>,
    before: Option<String>,
}

/// The super-admin sees every tenant; anyone else the tenants they belong to. Newest first, paged.
async fn list(
    State(st): State<AppState>,
    caller: Caller,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<tenants::Tenant>>, ApiError> {
    let Query(q) = q?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = TimeCursor::decode(q.before.as_deref())?;
    let rows = if caller.super_admin {
        tenants::list_all(&st.db, before, page::fetch(limit)).await?
    } else {
        tenants::list_for(&st.db, &caller.sub, before, page::fetch(limit)).await?
    };
    Ok(Json(Paged::new(rows, limit, |t| {
        TimeCursor::encode(t.ts_us, t.id)
    })))
}

/// The path's tenant, for a caller who must be its admin (or the super-admin).
async fn admin_of(st: &AppState, caller: Caller, id: &str) -> Result<TenantCtx, ApiError> {
    let ctx = tenancy::resolve(st, caller, tenancy::parse_tenant_id(id)?).await?;
    ctx.require(Role::Admin)?;
    Ok(ctx)
}

async fn members(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    Ok(paged(tenants::list_members(&st.db, ctx.tenant).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewMember {
    sub: String,
    role: Role,
}

/// Add a member; 409 `member_exists` when `sub` already belongs to the tenant (roles are not changed here).
async fn add_member(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    body: Result<Json<NewMember>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    let Json(b) = body?;
    let sub = clean("sub", &b.sub, MAX_SUB)?;
    let mut tx = st.db.begin().await?;
    let Some(m) = tenants::add_member(&mut tx, ctx.tenant, &sub, b.role).await? else {
        return Err(ApiError::Conflict(
            "member_exists",
            format!("{sub} is already a member of this tenant"),
        ));
    };
    let after = json!({"tenant": ctx.tenant, "sub": m.sub, "role": m.role});
    audit::record(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "member.add",
        &format!("member:{}:{}", ctx.tenant, m.sub),
        None,
        Some(&after),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(m))))
}

/// Remove a member; 409 `last_admin` when that would leave the tenant without an admin.
async fn remove_member(
    State(st): State<AppState>,
    caller: Caller,
    Path((id, sub)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    let mut tx = st.db.begin().await?;
    // Admin rows first, then the target: every removal takes the locks in the same order.
    let admins = tenants::lock_admins(&mut tx, ctx.tenant).await?;
    let Some(m) = tenants::lock_member(&mut tx, ctx.tenant, &sub).await? else {
        return Err(ApiError::Missing(
            "member_not_found",
            format!("{sub} is not a member of this tenant"),
        ));
    };
    if m.role == Role::Admin && admins.iter().all(|a| *a == m.sub) {
        return Err(ApiError::Conflict(
            "last_admin",
            "a tenant must keep at least one admin".into(),
        ));
    }
    tenants::remove_member(&mut tx, ctx.tenant, &m.sub).await?;
    let before = json!({"tenant": ctx.tenant, "sub": m.sub, "role": m.role});
    audit::record(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "member.remove",
        &format!("member:{}:{}", ctx.tenant, m.sub),
        Some(&before),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
