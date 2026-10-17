//! Tenants and their users (spec sections 10.1 and 10.2, rev 3). One tenant per identity.
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit;
use crate::auth::{Caller, require_super_admin};
use crate::db::{tenants, users};
use crate::error::ApiError;
use crate::page::{self, Paged, TimeCursor};
use crate::state::AppState;
use crate::tenancy::{self, Role, TenantCtx};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tenants", get(list).post(create))
        .route("/api/tenants/{id}/users", get(list_users).post(add_user))
        .route(
            "/api/tenants/{id}/users/{sub}",
            patch(change_role).delete(disable_user),
        )
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

fn user_exists(sub: &str) -> ApiError {
    ApiError::Conflict(
        "user_exists",
        format!("{sub} already belongs to a tenant (one tenant per identity)"),
    )
}

fn user_not_found(sub: &str) -> ApiError {
    ApiError::Missing(
        "user_not_found",
        format!("{sub} is not an active user of this tenant"),
    )
}

fn last_admin() -> ApiError {
    ApiError::Conflict(
        "last_admin",
        "a tenant must keep at least one active tenant_admin".into(),
    )
}

/// Super-admin only: mint the tenant id, create the tenant and its first `tenant_admin` in one
/// transaction (owner pool: platform work), audited. 409 `user_exists` when `first_admin_sub` already
/// belongs to a tenant; then nothing is created. Audit events name the caller by token `sub`.
async fn create(
    State(st): State<AppState>,
    caller: Caller,
    body: Result<Json<NewTenant>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_super_admin(&caller)?;
    let Json(b) = body?;
    let name = clean("name", &b.name, MAX_NAME)?;
    let first_admin_sub = clean("first_admin_sub", &b.first_admin_sub, MAX_SUB)?;
    let mut tx = st.owner.begin().await?;
    let Some(t) = tenants::create(&mut tx, &name, &first_admin_sub).await? else {
        return Err(user_exists(&first_admin_sub));
    };
    // W1.1: invite `first_admin_sub` in Keycloak through the strata-configurator-admin SA (needs the
    // gitops SA first).
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

/// The super-admin sees every tenant; anyone else their own (zero or one). Newest first, paged.
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

/// The path's tenant, for a caller who must be its `tenant_admin` (or the super-admin).
async fn admin_of(st: &AppState, caller: Caller, id: &str) -> Result<TenantCtx, ApiError> {
    let ctx = tenancy::in_tenant(st, caller, tenancy::parse_tenant_id(id)?).await?;
    ctx.require(Role::TenantAdmin)?;
    Ok(ctx)
}

async fn list_users(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    Ok(paged(users::list(&st.db, ctx.tenant).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewUser {
    sub: String,
    role: Role,
}

/// Add a user; 409 `user_exists` when `sub` already belongs to a tenant, this one or another (one
/// tenant per identity; roles are changed with PATCH).
async fn add_user(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    body: Result<Json<NewUser>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    let Json(b) = body?;
    let sub = clean("sub", &b.sub, MAX_SUB)?;
    let mut tx = st.db.begin().await?;
    let Some(u) = users::add(&mut tx, ctx.tenant, &sub, b.role).await? else {
        return Err(user_exists(&sub));
    };
    // W1.1: invite `sub` in Keycloak through the strata-configurator-admin SA (needs the gitops SA
    // first).
    let after = json!({"tenant": ctx.tenant, "sub": u.sub, "role": u.role});
    audit::record(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "user.add",
        &format!("user:{}:{}", ctx.tenant, u.sub),
        None,
        Some(&after),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(u))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleChange {
    role: Role,
}

/// Change an active user's role; 409 `last_admin` when that would leave no active `tenant_admin`.
async fn change_role(
    State(st): State<AppState>,
    caller: Caller,
    Path((id, sub)): Path<(String, String)>,
    body: Result<Json<RoleChange>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    let Json(b) = body?;
    let mut tx = st.db.begin().await?;
    // Admin rows first, then the target: every demotion and disable takes the locks in the same order.
    let admins = users::lock_admins(&mut tx, ctx.tenant).await?;
    let Some(u) = users::lock_active(&mut tx, ctx.tenant, &sub).await? else {
        return Err(user_not_found(&sub));
    };
    if u.role == Role::TenantAdmin
        && b.role != Role::TenantAdmin
        && admins.iter().all(|a| *a == u.sub)
    {
        return Err(last_admin());
    }
    let now = users::set_role(&mut tx, ctx.tenant, &u.sub, b.role).await?;
    audit::record(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "user.role",
        &format!("user:{}:{}", ctx.tenant, u.sub),
        Some(&json!({"tenant": ctx.tenant, "sub": u.sub, "role": u.role})),
        Some(&json!({"tenant": ctx.tenant, "sub": now.sub, "role": now.role})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!(now)))
}

/// Disable an active user (the row stays, so the identity stays bound to this tenant); 409
/// `last_admin` when that would leave no active `tenant_admin`.
async fn disable_user(
    State(st): State<AppState>,
    caller: Caller,
    Path((id, sub)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    let mut tx = st.db.begin().await?;
    let admins = users::lock_admins(&mut tx, ctx.tenant).await?;
    let Some(u) = users::lock_active(&mut tx, ctx.tenant, &sub).await? else {
        return Err(user_not_found(&sub));
    };
    if u.role == Role::TenantAdmin && admins.iter().all(|a| *a == u.sub) {
        return Err(last_admin());
    }
    users::disable(&mut tx, ctx.tenant, &u.sub).await?;
    let before = json!({"tenant": ctx.tenant, "sub": u.sub, "role": u.role});
    audit::record(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "user.disable",
        &format!("user:{}:{}", ctx.tenant, u.sub),
        Some(&before),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
