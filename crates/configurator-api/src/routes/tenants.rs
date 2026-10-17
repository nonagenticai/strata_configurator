//! Tenants and their users (spec sections 10.1 and 10.2, rev 3). One tenant per identity.
//!
//! People are named by `sub`, or by email to invite them (W1.1): the email resolves to the portal-realm
//! identity through the `strata-configurator-admin` SA (found, else created and sent Keycloak's action
//! email), whose id is the `sub`. Keycloak is called before any transaction opens (no network inside a
//! transaction). A tenant created while licence issuance is configured gets its licence first (W2.2).
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::audit;
use crate::auth::{Caller, require_super_admin};
use crate::db::{licenses, signups, tenants, users};
use crate::error::ApiError;
use crate::licenses_admin::{Issued, registry_name};
use crate::offboarding;
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
        .route("/api/tenants/{id}/offboard", post(offboard))
}

/// A whole (unpaged) list in the `Paged{items,next}` envelope: `next` is null.
fn paged<T: serde::Serialize>(items: Vec<T>) -> Json<Value> {
    Json(json!({"items": items, "next": null}))
}

/// `v` trimmed: 400 `invalid_body` when empty, longer than `max` characters, or holding control
/// characters. The trimmed value is what gets stored.
pub(crate) fn clean(field: &'static str, v: &str, max: usize) -> Result<String, ApiError> {
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

pub(crate) const MAX_NAME: usize = 200;
const MAX_SUB: usize = 255;
const MAX_EMAIL: usize = 254;

/// `v` as an email address: [`clean`], then one `@` with a non-empty local part and a dotted domain,
/// and no whitespace. Keycloak checks the rest.
pub(crate) fn email_address(field: &'static str, v: &str) -> Result<String, ApiError> {
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

/// Super-admin only: create a tenant and its first `tenant_admin` (by `first_admin_sub`, or invited by
/// `first_admin_email`); see [`create_tenant`]. 201 with the tenant.
async fn create(
    State(st): State<AppState>,
    caller: Caller,
    body: Result<Json<NewTenant>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_super_admin(&caller)?;
    let Json(b) = body?;
    let name = clean("name", &b.name, MAX_NAME)?;
    let created = create_tenant(
        &st,
        &caller.sub,
        &name,
        (b.first_admin_sub, b.first_admin_email),
        Origin::Manual,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(created.tenant))))
}

/// What asked for a tenant: a super-admin's `POST /api/tenants`, or their approval of an early-access
/// signup (W2.3), which is marked approved in the same transaction as the tenant.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Origin {
    Manual,
    Signup(Uuid),
}

/// A tenant [`create_tenant`] created.
pub(crate) struct Created {
    pub tenant: tenants::Tenant,
    pub first_admin_sub: String,
    /// The first admin's identity was created (and sent the invite email) by this call.
    pub invited: bool,
    /// The approved signup, for [`Origin::Signup`].
    pub signup: Option<signups::Approval>,
}

/// The one tenant-create path (the caller has checked it is the super-admin and cleaned `name`): mint
/// the tenant id, resolve the first admin (`(sub, email)`, exactly one; see [`subject`]), create the
/// tenant and its first `tenant_admin` in one transaction (owner pool: platform work), audited. 409
/// `user_exists` when the first admin already belongs to a tenant; then nothing is created. Audit events
/// name the caller by token `sub`. For [`Origin::Signup`] the signup is locked first and marked approved
/// in the same transaction (409 `approval_not_pending` when it was decided meanwhile).
///
/// With licence issuance configured, the licence is issued for the new id, held by the first admin,
/// BEFORE the transaction, and stored in it. As in commonpractice (`tenant-service.ts` `createTenant`),
/// a failed issuance fails the create: 502 `licences_unavailable`, nothing is created, and the failure
/// is audited as `license.issue_failed`. A licence issued for a create that fails before its commit is
/// revoked again; when the commit itself fails it may still have landed, so the licence is kept and
/// the pair logged for reconciliation by hand (revoking could leave a live tenant unlicensed).
pub(crate) async fn create_tenant(
    st: &AppState,
    actor: &str,
    name: &str,
    (sub, email): (Option<String>, Option<String>),
    origin: Origin,
) -> Result<Created, ApiError> {
    let (first_admin_sub, invited) =
        subject(st, ("first_admin_sub", sub), ("first_admin_email", email)).await?;
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
                .issue(id, &registry_name(name, id), &first_admin_sub)
                .await
            {
                Ok(l) => Some(l),
                Err(e) => {
                    tracing::error!(tenant = %id, error = %e, "licence issuance failed; tenant not created");
                    let attempted = json!({"tenant": id, "holder_sub": first_admin_sub});
                    audit::record(
                        &mut *st.owner.acquire().await?,
                        actor,
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

    let staged = insert_tenant(
        st,
        actor,
        id,
        name,
        &first_admin_sub,
        licence.as_ref(),
        origin,
    )
    .await;
    let (tenant, signup, tx) = match staged {
        Ok(Some(staged)) => staged,
        Ok(None) => {
            give_back(st, id, licence.as_ref()).await;
            kept_identity(&first_admin_sub, invited);
            return Err(user_exists(&first_admin_sub));
        }
        Err(e) => {
            give_back(st, id, licence.as_ref()).await;
            kept_identity(&first_admin_sub, invited);
            return Err(e);
        }
    };
    if let Err(e) = tx.commit().await {
        // The commit may have landed even though it reported an error: revoking here could leave a
        // live tenant without its licence, so keep it and leave the pair for a person to reconcile.
        if let Some(l) = &licence {
            tracing::error!(tenant = %id, license_id = %l.license_id, error = %e,
                "tenant create commit failed and may have landed; licence NOT revoked, reconcile by hand");
        }
        return Err(ApiError::from(e));
    }
    Ok(Created {
        tenant,
        first_admin_sub,
        invited,
        signup,
    })
}

/// 409 `approval_not_pending`: the signup was approved or rejected already.
pub(crate) fn approval_not_pending(id: Uuid) -> ApiError {
    ApiError::Conflict(
        "approval_not_pending",
        format!("signup {id} is no longer pending"),
    )
}

/// Revoke the licence issued for tenant `id`, whose create failed before its commit (best effort; a
/// failed revoke is logged for cleanup by hand).
async fn give_back(st: &AppState, id: Uuid, licence: Option<&Issued>) {
    if let (Some(la), Some(l)) = (st.licenses_admin.as_deref(), licence)
        && let Err(e) = la.revoke(id, &l.license_id).await
    {
        tracing::error!(tenant = %id, license_id = %l.license_id, error = %e,
            "could not revoke the licence of a tenant that was not created; revoke it by hand");
    }
}

/// The tenant, its first admin, its licence (when one was issued), the approved signup (for
/// [`Origin::Signup`]) and their audit events, staged in one owner transaction that the caller commits
/// (so it can tell a commit failure, which may have landed, from a failure before it). `None` when the
/// first admin already belongs to a tenant.
type Staged = (
    tenants::Tenant,
    Option<signups::Approval>,
    Transaction<'static, Postgres>,
);

async fn insert_tenant(
    st: &AppState,
    actor: &str,
    id: Uuid,
    name: &str,
    first_admin_sub: &str,
    licence: Option<&Issued>,
    origin: Origin,
) -> Result<Option<Staged>, ApiError> {
    let mut tx = st.owner.begin().await?;
    // The signup first: its row lock serialises concurrent decisions on it before anything else is
    // written, so the second one is a clean 409 rather than a `user_exists` from the users insert.
    if let Origin::Signup(sid) = origin
        && signups::lock_pending(&mut tx, sid).await?.is_none()
    {
        return Err(approval_not_pending(sid));
    }
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
        // Config refuses to start issuance without the key, so a missing cipher is unreachable.
        let cipher = st.token_cipher.as_deref().ok_or_else(|| {
            ApiError::Internal(anyhow::anyhow!(
                "a licence was issued but LICENSE_TOKEN_ENC_KEY is not configured"
            ))
        })?;
        let sealed = cipher
            .seal(t.id, &l.token)
            .map_err(|e| ApiError::Internal(anyhow::Error::from(e)))?;
        licenses::store(&mut tx, t.id, l, first_admin_sub, &sealed).await?;
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
    let signup = match origin {
        Origin::Manual => None,
        Origin::Signup(sid) => {
            let Some(a) = signups::approve(&mut tx, sid, t.id, actor).await? else {
                return Err(approval_not_pending(sid));
            };
            let before = json!({"signup": sid, "status": "pending"});
            let after = json!({"signup": sid, "status": a.status, "tenant": t.id,
                               "first_admin_sub": first_admin_sub});
            audit::record(
                &mut tx,
                actor,
                Some(t.id),
                "signup.approve",
                &format!("signup:{sid}"),
                Some(&before),
                Some(&after),
            )
            .await?;
            Some(a)
        }
    };
    Ok(Some((t, signup, tx)))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<String>,
    before: Option<String>,
    include_archived: Option<String>,
}

/// `include_archived`: absent or `false` → false, `true` → true; anything else 400 `invalid_query`.
fn include_archived(v: Option<&str>) -> Result<bool, ApiError> {
    match v {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(ApiError::BadRequest(
            "invalid_query",
            "include_archived must be true or false".into(),
        )),
    }
}

/// The super-admin sees every live tenant (and the archived ones too with `include_archived=true`);
/// anyone else their own (zero or one; never an archived one). Newest first, paged.
async fn list(
    State(st): State<AppState>,
    caller: Caller,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<tenants::Tenant>>, ApiError> {
    let Query(q) = q?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = TimeCursor::decode(q.before.as_deref())?;
    let archived = include_archived(q.include_archived.as_deref())?;
    let rows = if caller.super_admin {
        tenants::list_all(&st.db, before, page::fetch(limit), archived).await?
    } else {
        tenants::list_for(&st.db, &caller.sub, before, page::fetch(limit)).await?
    };
    Ok(Json(Paged::new(rows, limit, |t| {
        TimeCursor::encode(t.ts_us, t.id)
    })))
}

/// The path's tenant, for a caller who must be its `tenant_admin` (or the super-admin).
async fn admin_of(
    st: &AppState,
    caller: Caller,
    headers: &HeaderMap,
    id: &str,
) -> Result<TenantCtx, ApiError> {
    let ctx = tenancy::in_tenant(st, caller, headers, tenancy::parse_tenant_id(id)?).await?;
    ctx.require(Role::TenantAdmin)?;
    Ok(ctx)
}

async fn list_users(
    State(st): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ctx = admin_of(&st, caller, &headers, &id).await?;
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
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<NewUser>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = admin_of(&st, caller, &headers, &id).await?;
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
    headers: HeaderMap,
    Path((id, sub)): Path<(String, String)>,
    body: Result<Json<RoleChange>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let ctx = admin_of(&st, caller, &headers, &id).await?;
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
    headers: HeaderMap,
    Path((id, sub)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let ctx = admin_of(&st, caller, &headers, &id).await?;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OffboardBody {
    /// The tenant's name, typed by the operator (commonpractice types the slug; this tenant has none).
    confirm_name: String,
}

/// Super-admin only, a platform route (not gated by licence gate 2 for the target tenant; as
/// commonpractice's `requireSuperAdmin` `POST /api/tenants/:id/offboard`): offboard the tenant, see
/// [`offboarding`]. Guards first, before any write: 403 `forbidden` (not the super-admin), 400
/// `invalid_tenant`, 400 `invalid_body`, 404 `tenant_not_found`, 400 `confirm_mismatch` unless
/// `confirm_name` is the tenant's name. Then the licence is revoked (502 `license_provisioning_failed`
/// when that fails: nothing else changed, the tenant is live and still licensed, retry), then the tenant
/// is archived and its users disabled, audited as `tenant.offboard`. 200 with the archived tenant.
///
/// Idempotent: offboarding an archived tenant is 200 with the same tenant (its original `archived_at`),
/// `already_archived: true` and `license_revoked: null`, without calling licences; nothing is written
/// (unless a user was somehow left active, who is disabled then).
async fn offboard(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    body: Result<Json<OffboardBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    require_super_admin(&caller)?;
    let tenant = tenancy::parse_tenant_id(&id)?;
    let Json(b) = body?;
    let Some(t) = tenants::get(&st.db, tenant).await? else {
        return Err(tenancy::tenant_not_found(tenant));
    };
    if b.confirm_name.trim() != t.name {
        return Err(ApiError::BadRequest(
            "confirm_mismatch",
            format!(
                "type the tenant name {:?} to confirm offboarding; this cannot be undone",
                t.name
            ),
        ));
    }
    // Archived already: the licence was revoked before it was archived, so licences is not asked
    // again; the archive below then changes nothing.
    let revoked = if t.archived_at.is_some() {
        None
    } else {
        offboarding::revoke_tenant_license(&st, &caller.sub, tenant).await?
    };
    let done = offboarding::archive_tenant(&st, &caller.sub, tenant, revoked.as_deref()).await?;
    Ok(Json(json!({
        "tenant": done.tenant,
        "already_archived": done.already_archived,
        "license_revoked": revoked,
        "users_disabled": done.users_disabled,
    })))
}
