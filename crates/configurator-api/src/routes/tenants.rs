//! Tenants and their members (spec sections 10.1 and 10.2).
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit;
use crate::auth::{Caller, require_super_admin};
use crate::db::tenants;
use crate::error::ApiError;
use crate::state::AppState;
use crate::tenancy::{self, Role, TenantCtx};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tenants", get(list).post(create))
        .route("/api/tenants/{id}/members", get(members).post(add_member))
        .route("/api/tenants/{id}/members/{sub}", delete(remove_member))
}

/// A list in the `Paged{items,next}` envelope. Paging arrives with the shared helper; `next` is null.
fn paged<T: serde::Serialize>(items: Vec<T>) -> Json<Value> {
    Json(json!({"items": items, "next": null}))
}

fn non_empty(field: &'static str, v: &str) -> Result<(), ApiError> {
    if v.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "invalid_body",
            format!("{field} must not be empty"),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTenant {
    name: String,
    first_admin_sub: String,
}

/// Super-admin only: the tenant and its first admin, in one transaction, audited.
async fn create(
    State(st): State<AppState>,
    caller: Caller,
    body: Result<Json<NewTenant>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_super_admin(&caller)?;
    let Json(b) = body?;
    non_empty("name", &b.name)?;
    non_empty("first_admin_sub", &b.first_admin_sub)?;
    let mut tx = st.db.begin().await?;
    let t = tenants::create(&mut tx, b.name.trim(), &b.first_admin_sub).await?;
    let after = json!({"tenant": t, "first_admin_sub": b.first_admin_sub});
    audit::record(
        &mut tx,
        &caller.actor,
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

/// The super-admin sees every tenant; anyone else the tenants they belong to.
async fn list(State(st): State<AppState>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let items = if caller.super_admin {
        tenants::list_all(&st.db).await?
    } else {
        tenants::list_for(&st.db, &caller.sub).await?
    };
    Ok(paged(items))
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
    non_empty("sub", &b.sub)?;
    let mut tx = st.db.begin().await?;
    let Some(m) = tenants::add_member(&mut tx, ctx.tenant, &b.sub, b.role).await? else {
        return Err(ApiError::Conflict(
            "member_exists",
            format!("{} is already a member of this tenant", b.sub),
        ));
    };
    let after = json!({"tenant": ctx.tenant, "sub": m.sub, "role": m.role});
    audit::record(
        &mut tx,
        &ctx.caller.actor,
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
        &ctx.caller.actor,
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
