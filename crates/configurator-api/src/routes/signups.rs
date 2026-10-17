//! Early-access self-signup (W2.3, spec section 10.7), after commonpractice `routes/onboarding.ts`
//! (`intakeSignup`, `listApprovals`, `approveSignup`, `rejectSignup`; patterns only, no shared code).
//! Mounted only while `EARLY_ACCESS_SIGNUP_ENABLED` (otherwise every path here is the unknown-route
//! 404), see [`super::router`].
//!
//! - `POST /api/signup-approvals/intake`: the website or chat backend, server to server, with a
//!   client-credentials token (see [`signup::require_intake_caller`]). 202 `{"status":"queued"}` for a
//!   new pending row, 202 `{"status":"already_pending"}` when the address already waits. The row holds
//!   no `sub` (the service account is not the requester), so approval invites the address.
//! - `GET /api/signup-approvals`, `POST /api/signup-approvals/{id}/approve|reject`: the super-admin.
//!   Approval runs the same tenant-create path as `POST /api/tenants`
//!   ([`super::tenants::create_tenant`]): Keycloak invite, licence issuance, revoke on failure, audit.
//!
//! The queue is platform data: every query runs on the owner pool (the runtime role has no grant).
use axum::body::Bytes;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::tenants::{MAX_NAME, Origin, approval_not_pending, clean, create_tenant, email_address};
use crate::audit;
use crate::auth::{Caller, require_super_admin};
use crate::db::signups::{self, NewSignup};
use crate::error::ApiError;
use crate::page::{self, Paged, TimeCursor};
use crate::signup::{
    default_tenant_name, multi_line_field, require_intake_caller, single_line_field,
};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/signup-approvals", get(list))
        .route("/api/signup-approvals/intake", post(intake))
        .route("/api/signup-approvals/{id}/approve", post(approve))
        .route("/api/signup-approvals/{id}/reject", post(reject))
}

/// Requester-typed limits (commonpractice `intakeSchema`).
const MAX_TEXT: usize = 160;
const MAX_MESSAGE: usize = 2000;
const MAX_TURNSTILE_TOKEN: usize = 4096;
/// A workspace name given at approval (commonpractice `tenantName`: 2 to 120 characters).
const MAX_TENANT_NAME: usize = 120;

/// What the website backend or the signup chat may send. Strict, camelCase on the wire as in
/// commonpractice, so one client can call both apps.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Intake {
    email: String,
    name: String,
    company: Option<String>,
    message: Option<String>,
    source: String,
    turnstile_token: Option<String>,
}

fn invalid(msg: &str) -> ApiError {
    ApiError::BadRequest("invalid_body", msg.into())
}

/// Queue an early-access signup (see the module doc). Validation is commonpractice's: `email` an
/// address, `name` (required) and `company` one printable line of at most 160 characters, `message` at
/// most 2000 characters with its line breaks kept, `source` `web` or `chat`, `turnstileToken` 1 to
/// 4096 characters and verified when `TURNSTILE_SECRET` is set. 503 `signup_queue_full` when
/// `SIGNUP_QUEUE_MAX_PENDING` rows are pending. A new row is announced in Telegram when that is on; a
/// failed notice never fails the intake.
async fn intake(
    State(st): State<AppState>,
    caller: Caller,
    body: Result<Json<Intake>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let cfg = &st.signup.cfg;
    require_intake_caller(cfg, &caller)?;
    let Json(b) = body?;
    let email = email_address("email", &b.email)?;
    let name = single_line_field("name", &b.name, MAX_TEXT)?;
    let company = b
        .company
        .as_deref()
        .map(|c| single_line_field("company", c, MAX_TEXT))
        .transpose()?;
    let message = match b.message.as_deref() {
        Some(m) => multi_line_field("message", m, MAX_MESSAGE)?,
        None => None,
    };
    let source = match b.source.as_str() {
        "web" | "chat" => b.source.clone(),
        _ => return Err(invalid("source must be web or chat")),
    };
    if let Some(t) = &b.turnstile_token
        && (t.is_empty() || t.chars().count() > MAX_TURNSTILE_TOKEN)
    {
        return Err(invalid(&format!(
            "turnstileToken must be 1 to {MAX_TURNSTILE_TOKEN} characters"
        )));
    }
    st.signup
        .verify_turnstile(b.turnstile_token.as_deref())
        .await?;

    // The SA's client id is recorded (and is in the audit event); its `sub` is not the requester's.
    let client = caller.azp.clone().unwrap_or_default();
    let mut tx = st.owner.begin().await?;
    let pending = signups::pending_count(&mut tx).await?;
    if pending >= cfg.max_pending {
        tracing::warn!(
            pending,
            cap = cfg.max_pending,
            intake_client = %client,
            "signup_queue_full: refusing new signups until the queue is worked down"
        );
        return Err(ApiError::Unavailable(
            "signup_queue_full",
            "sign-up requests are temporarily closed; try again later".into(),
        ));
    }
    let new = NewSignup {
        email: &email,
        name: &name,
        company: company.as_deref(),
        message: message.as_deref(),
        source: &source,
        intake_client: &client,
    };
    let Some(id) = signups::insert(&mut tx, &new).await? else {
        tx.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"status": "already_pending"})),
        ));
    };
    // Ids only: the requester's address and name never reach the audit metadata (hashes only anyway).
    let after = json!({"signup": id, "source": source, "intake_client": client});
    audit::record(
        &mut tx,
        &caller.sub,
        None,
        "signup.intake",
        &format!("signup:{id}"),
        None,
        Some(&after),
    )
    .await?;
    tx.commit().await?;
    st.signup.notify_new(id, &source, Some(pending + 1));
    Ok((StatusCode::ACCEPTED, Json(json!({"status": "queued"}))))
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    limit: Option<String>,
    before: Option<String>,
}

/// The queue, newest first, paged; `?status=pending|approved|rejected` narrows it. Each item carries
/// `suggested_tenant_name`, the name approval uses when none is given.
async fn list(
    State(st): State<AppState>,
    caller: Caller,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    require_super_admin(&caller)?;
    let Query(q) = q?;
    if let Some(s) = q.status.as_deref()
        && !matches!(s, "pending" | "approved" | "rejected")
    {
        return Err(ApiError::BadRequest(
            "invalid_query",
            "status must be pending, approved or rejected".into(),
        ));
    }
    let limit = page::limit(q.limit.as_deref())?;
    let before = TimeCursor::decode(q.before.as_deref())?;
    let rows = signups::list(&st.owner, q.status.as_deref(), before, page::fetch(limit)).await?;
    let paged = Paged::new(rows, limit, |a| TimeCursor::encode(a.ts_us, a.id));
    let items: Vec<Value> = paged
        .items
        .into_iter()
        .map(|a| {
            let suggested =
                default_tenant_name(a.company.as_deref(), &a.email, Some(a.name.as_str()));
            let mut v = json!(a);
            v["suggested_tenant_name"] = json!(suggested);
            v
        })
        .collect();
    Ok(Json(json!({"items": items, "next": paged.next})))
}

fn approval_not_found(id: &str) -> ApiError {
    ApiError::Missing("approval_not_found", format!("no signup approval {id:?}"))
}

/// The path's signup id: a uuid, else 404 `approval_not_found` (it cannot name a signup).
fn signup_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| approval_not_found(raw))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApproveBody {
    tenant_name: Option<String>,
}

/// Approve a pending signup: create its tenant with the requester invited by email as the first
/// `tenant_admin`, through [`create_tenant`] (Keycloak invite, licence on the configured plan when
/// issuance is on, revoke on failure, `tenant.create` + `license.issue` audit), and mark the signup
/// approved with `signup.approve` in the same transaction. The body is optional: `{"tenantName"}`, else
/// [`default_tenant_name`]. 404 `approval_not_found`; 409 `approval_not_pending` when it was decided
/// already, including by a concurrent approval or rejection; 409 `user_exists` when the requester
/// already belongs to a tenant; 503 `invite_unavailable` without the Keycloak admin SA.
async fn approve(
    State(st): State<AppState>,
    caller: Caller,
    Path(raw): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    require_super_admin(&caller)?;
    let id = signup_id(&raw)?;
    // Optional: no body, `null` and `{}` all mean "use the suggested name".
    let b: ApproveBody = if body.iter().all(u8::is_ascii_whitespace) {
        ApproveBody::default()
    } else {
        serde_json::from_slice::<Option<ApproveBody>>(&body)
            .map_err(|e| invalid(&e.to_string()))?
            .unwrap_or_default()
    };
    // A cheap check before any network call; the transaction re-checks it under a row lock.
    let Some(a) = signups::get(&mut *st.owner.acquire().await?, id).await? else {
        return Err(approval_not_found(&raw));
    };
    if a.status != "pending" {
        return Err(approval_not_pending(id));
    }
    let name = match b.tenant_name.as_deref() {
        Some(n) => {
            let n = clean("tenantName", n, MAX_TENANT_NAME)?;
            if n.chars().count() < 2 {
                return Err(invalid("tenantName must be at least 2 characters"));
            }
            n
        }
        None => clean(
            "tenantName",
            &default_tenant_name(a.company.as_deref(), &a.email, Some(a.name.as_str())),
            MAX_NAME,
        )?,
    };
    let created = create_tenant(
        &st,
        &caller.sub,
        &name,
        (None, Some(a.email.clone())),
        Origin::Signup(id),
    )
    .await?;
    // W4: provision the new tenant down into care-bus here (spec 10.7: approval "provisions the tenant
    // down into care-bus once care-bus accepts it"). Not built: care-bus W3 has not landed.
    st.signup.notify_decision(id, Some(created.tenant.id));
    Ok(Json(json!({
        "approval": created.signup,
        "tenant": created.tenant,
        "first_admin_sub": created.first_admin_sub,
        "invited": created.invited,
    })))
}

/// Reject a pending signup (no Keycloak call), audited as `signup.reject`. 404 `approval_not_found`;
/// 409 `approval_not_pending` when it was decided already.
async fn reject(
    State(st): State<AppState>,
    caller: Caller,
    Path(raw): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_super_admin(&caller)?;
    let id = signup_id(&raw)?;
    let mut tx = st.owner.begin().await?;
    let Some(a) = signups::reject(&mut tx, id, &caller.sub).await? else {
        return Err(match signups::get(&mut tx, id).await? {
            Some(_) => approval_not_pending(id),
            None => approval_not_found(&raw),
        });
    };
    audit::record(
        &mut tx,
        &caller.sub,
        None,
        "signup.reject",
        &format!("signup:{id}"),
        Some(&json!({"signup": id, "status": "pending"})),
        Some(&json!({"signup": id, "status": a.status})),
    )
    .await?;
    tx.commit().await?;
    st.signup.notify_decision(id, None);
    Ok(Json(json!({"approval": a})))
}
