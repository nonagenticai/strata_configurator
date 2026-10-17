//! Tenants and their users (spec sections 10.1 and 10.2, rev 3). One tenant per identity.
//!
//! People are named by `sub`, or by email to invite them (W1.1): the email resolves to the portal-realm
//! identity through the `strata-configurator-admin` SA (found, else created and sent Keycloak's action
//! email), whose id is the `sub`. Keycloak is called before any transaction opens (no network inside a
//! transaction). A tenant created while licence issuance is configured gets its licence first (W2.2).
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::audit;
use crate::auth::{Caller, require_super_admin};
use crate::db::{licenses, tenants, users};
use crate::error::ApiError;
use crate::licenses_admin::{Issued, registry_name};
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
const MAX_EMAIL: usize = 254;

/// `v` as an email address: [`clean`], then one `@` with a non-empty local part and a dotted domain,
/// and no whitespace. Keycloak checks the rest.
fn email_address(field: &'static str, v: &str) -> Result<String, ApiError> {
    let e = clean(field, v, MAX_EMAIL)?;
    let ok = !e.chars().any(char::is_whitespace)
        && e.matches('@').count() == 1
        && e.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if !ok {
        return Err(ApiError::BadRequest(
            "invalid_body",
            format!("{field} must be an email address"),
        ));
    }
    Ok(e)
}

/// The person a body names, by exactly one of `sub` (as given) or `email` (400 `invalid_body`
/// otherwise). An email is resolved in Keycloak: the existing identity (no email sent), else a new one
/// that is sent the action email; 503 `invite_unavailable` when the admin SA is not configured, 502
/// `keycloak_admin_failed` when Keycloak fails. Returns the `sub` and whether this call created it.
async fn subject(
    st: &AppState,
    (sub_field, sub): (&'static str, Option<String>),
    (email_field, email): (&'static str, Option<String>),
) -> Result<(String, bool), ApiError> {
    match (sub, email) {
        (Some(s), None) => Ok((clean(sub_field, &s, MAX_SUB)?, false)),
        (None, Some(e)) => {
            let e = email_address(email_field, &e)?;
            let Some(kc) = st.keycloak_admin.as_deref() else {
                return Err(ApiError::Unavailable(
                    "invite_unavailable",
                    "inviting by email is not configured here; give a sub".into(),
                ));
            };
            let invited = kc.find_or_invite(&e).await.map_err(|err| {
                tracing::error!(error = %err, "keycloak invite failed");
                ApiError::BadGateway(
                    "keycloak_admin_failed",
                    "the identity provider did not complete the invite".into(),
                )
            })?;
            Ok((clean(sub_field, &invited.sub, MAX_SUB)?, invited.created))
        }
        _ => Err(ApiError::BadRequest(
            "invalid_body",
            format!("give exactly one of {sub_field} and {email_field}"),
        )),
    }
}

/// The DB insert after a new identity was invited failed: the identity stays (it is shared with every
/// app in the portal realm and may be wanted elsewhere), so say so.
fn kept_identity(sub: &str, created: bool) {
    if created {
        tracing::warn!(%sub, "invited keycloak identity kept although the user was not added");
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTenant {
    name: String,
    first_admin_sub: Option<String>,
    first_admin_email: Option<String>,
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

/// Super-admin only: mint the tenant id, create the tenant and its first `tenant_admin` (by
/// `first_admin_sub`, or invited by `first_admin_email`) in one transaction (owner pool: platform work),
/// audited. 409 `user_exists` when the first admin already belongs to a tenant; then nothing is
/// created. Audit events name the caller by token `sub`.
///
/// With licence issuance configured, the licence is issued for the new id, held by the first admin,
/// BEFORE the transaction, and stored in it. As in commonpractice (`tenant-service.ts` `createTenant`),
/// a failed issuance fails the create: 502 `licences_unavailable`, nothing is created, and the failure
/// is audited as `license.issue_failed`. A licence issued for a create that then does not commit is
/// revoked again.
async fn create(
    State(st): State<AppState>,
    caller: Caller,
    body: Result<Json<NewTenant>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_super_admin(&caller)?;
    let Json(b) = body?;
    let name = clean("name", &b.name, MAX_NAME)?;
    let (first_admin_sub, invited) = subject(
        &st,
        ("first_admin_sub", b.first_admin_sub),
        ("first_admin_email", b.first_admin_email),
    )
    .await?;
    let id = Uuid::new_v4();

    let licence = match st.licenses_admin.as_deref() {
        None => None,
        Some(la) => {
            // Refuse a taken identity before a licence is minted for a tenant that would not exist.
            if users::exists(&st.db, &first_admin_sub).await? {
                kept_identity(&first_admin_sub, invited);
                return Err(user_exists(&first_admin_sub));
            }
            match la
                .issue(id, &registry_name(&name, id), &first_admin_sub)
                .await
            {
                Ok(l) => Some(l),
                Err(e) => {
                    tracing::error!(tenant = %id, error = %e, "licence issuance failed; tenant not created");
                    let attempted = json!({"tenant": id, "holder_sub": first_admin_sub});
                    audit::record(
                        &mut *st.owner.acquire().await?,
                        &caller.sub,
                        Some(id),
                        "license.issue_failed",
                        &format!("tenant:{id}"),
                        None,
                        Some(&attempted),
                    )
                    .await?;
                    return Err(ApiError::BadGateway(
                        "licences_unavailable",
                        "licensing the new tenant failed, so it was not created".into(),
                    ));
                }
            }
        }
    };

    let created = insert_tenant(
        &st,
        &caller.sub,
        id,
        &name,
        &first_admin_sub,
        licence.as_ref(),
    )
    .await;
    if !matches!(created, Ok(Some(_)))
        && let (Some(la), Some(l)) = (st.licenses_admin.as_deref(), &licence)
    {
        // The tenant was not created: give back the licence minted for it.
        if let Err(e) = la.revoke(id, &l.license_id).await {
            tracing::error!(tenant = %id, license_id = %l.license_id, error = %e,
                "could not revoke the licence of a tenant that was not created; revoke it by hand");
        }
    }
    let Some(t) = created? else {
        kept_identity(&first_admin_sub, invited);
        return Err(user_exists(&first_admin_sub));
    };
    Ok((StatusCode::CREATED, Json(json!(t))))
}

/// The tenant, its first admin, its licence (when one was issued) and their audit events, in one
/// owner transaction. `None` when the first admin already belongs to a tenant.
async fn insert_tenant(
    st: &AppState,
    actor: &str,
    id: Uuid,
    name: &str,
    first_admin_sub: &str,
    licence: Option<&Issued>,
) -> Result<Option<tenants::Tenant>, ApiError> {
    let mut tx = st.owner.begin().await?;
    let Some(t) = tenants::create(&mut tx, id, name, first_admin_sub).await? else {
        return Ok(None);
    };
    let after = json!({"tenant": t, "first_admin_sub": first_admin_sub});
    audit::record(
        &mut tx,
        actor,
        Some(t.id),
        "tenant.create",
        &format!("tenant:{}", t.id),
        None,
        Some(&after),
    )
    .await?;
    if let Some(l) = licence {
        licenses::store(&mut tx, t.id, l, first_admin_sub).await?;
        let after = json!({"tenant": t.id, "license_id": l.license_id, "subject_id": l.subject_id,
                           "holder_sub": first_admin_sub});
        audit::record(
            &mut tx,
            actor,
            Some(t.id),
            "license.issue",
            &format!("license:{}", l.license_id),
            None,
            Some(&after),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Some(t))
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
    sub: Option<String>,
    email: Option<String>,
    role: Role,
}

/// Add a user by `sub`, or invite one by `email` (see [`subject`]); 409 `user_exists` when the person
/// already belongs to a tenant, this one or another (one tenant per identity; roles are changed with
/// PATCH). An identity invited for an add that then fails is kept (logged), never deleted.
async fn add_user(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    body: Result<Json<NewUser>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = admin_of(&st, caller, &id).await?;
    let Json(b) = body?;
    let (sub, invited) = subject(&st, ("sub", b.sub), ("email", b.email)).await?;
    let mut tx = st.db.begin().await?;
    let Some(u) = users::add(&mut tx, ctx.tenant, &sub, b.role).await? else {
        kept_identity(&sub, invited);
        return Err(user_exists(&sub));
    };
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
/// `last_admin` when that would leave no active `tenant_admin`. The Keycloak identity is NOT disabled:
/// the portal realm is shared, and the same person may use other apps.
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
