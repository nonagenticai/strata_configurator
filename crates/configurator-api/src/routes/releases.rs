//! Release targets and versioned releases (W4, configurator plan 03 Tasks 6-8).
//!
//! A target is one job of the tenant with a stored selection. Building compiles it, turns the
//! effective config into a care-bus release bundle v1 and stores the PUT body as release
//! `cfg-YYYY-MM-DD.N`; a build that changes nothing returns the latest release. Everything runs in the
//! tenant's transaction (RLS).
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use configurator_core::release::{BuildError, BundleV1, build_bundle, bundle_hash, release_body};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::audit;
use crate::carebus_client::CarebusError;
use crate::db::releases::{self as db, NewRelease, NewStep, Release, Target};
use crate::db::tenants;
use crate::error::ApiError;
use crate::page::{self, Paged, TimeCursor};
use crate::provision::{self, StepOutcome};
use crate::routes::compile::{SelectionRequest, compile_selection};
use crate::state::AppState;
use crate::tenancy::{self, Role, TenantCtx};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/targets", get(list_targets).post(create_target))
        .route(
            "/api/targets/{id}/releases",
            get(list_releases).post(build_release),
        )
        .route("/api/releases/{id}", get(show_release))
        .route("/api/releases/{id}/push", post(push))
        .route("/api/targets/{id}/drift", get(drift))
}

fn internal(e: impl Into<anyhow::Error>) -> ApiError {
    ApiError::Internal(e.into())
}

fn target_not_found(id: &str) -> ApiError {
    ApiError::Missing("target_not_found", format!("target {id} does not exist"))
}

fn release_not_found(id: &str) -> ApiError {
    ApiError::Missing("release_not_found", format!("release {id} does not exist"))
}

/// The target `id` of the tenant `tx` acts in; a malformed id or another tenant's target is the same
/// 404 as an unknown one.
async fn target_of(tx: &mut PgConnection, id: &str) -> Result<Target, ApiError> {
    let uuid = Uuid::parse_str(id).map_err(|_| target_not_found(id))?;
    db::get_target(tx, uuid)
        .await?
        .ok_or_else(|| target_not_found(id))
}

async fn list_targets(State(st): State<AppState>, ctx: TenantCtx) -> Result<Json<Value>, ApiError> {
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let items = db::list_targets(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"items": items, "next": null})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTarget {
    job: String,
    #[serde(default)]
    selection: SelectionRequest,
}

/// `POST /api/targets` (tenant_admin): the selection must compile now (400 `invalid_selection`, 422
/// `compile_failed`); one target per job (409 `target_exists`).
async fn create_target(
    State(st): State<AppState>,
    ctx: TenantCtx,
    body: Result<Json<NewTarget>, JsonRejection>,
) -> Result<(StatusCode, Json<Target>), ApiError> {
    ctx.require(Role::TenantAdmin)?;
    let Json(b) = body?;
    let mut selection = b.selection;
    if selection.job.as_ref().is_some_and(|j| *j != b.job) {
        return Err(ApiError::BadRequest(
            "invalid_selection",
            "selection.job must be omitted (the target's job is used)".into(),
        ));
    }
    selection.job = None;
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let mut probe = selection.clone();
    probe.job = Some(b.job.clone());
    compile_selection(&mut tx, Some(ctx.tenant), probe).await?;
    let stored = serde_json::to_value(&selection).map_err(internal)?;
    let target =
        match db::insert_target(&mut tx, ctx.tenant, &b.job, &stored, &ctx.caller.sub).await {
            Ok(t) => t,
            Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
                return Err(ApiError::Conflict(
                    "target_exists",
                    format!("this tenant already has a target for job {}", b.job),
                ));
            }
            Err(e) => return Err(e.into()),
        };
    audit::record(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "target.create",
        &format!("release_target:{}", target.id),
        None,
        Some(&stored),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(target)))
}

/// What one build of a target yields.
struct Built {
    bundle: BundleV1,
    hash: String,
    effective_hash: String,
    job_label: String,
}

fn build_error(e: BuildError) -> ApiError {
    let (code, violations) = match e {
        BuildError::Incomplete(v) => ("bundle_incomplete", v),
        BuildError::Invalid(v) => ("bundle_invalid", v),
    };
    ApiError::Detailed {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code,
        message: format!("{} problem(s); see errors", violations.len()),
        errors: violations.iter().map(|v| json!(v)).collect(),
    }
}

/// Compile the target's selection with its job and build the release bundle.
async fn build(tx: &mut PgConnection, tenant: Uuid, target: &Target) -> Result<Built, ApiError> {
    let mut sel: SelectionRequest =
        serde_json::from_value(target.selection.clone()).map_err(internal)?;
    sel.job = Some(target.job.clone());
    let cfg = compile_selection(tx, Some(tenant), sel).await?;
    let bundle = build_bundle(&cfg).map_err(build_error)?;
    let hash = bundle_hash(&serde_json::to_value(&bundle).map_err(internal)?).map_err(internal)?;
    let job_label = cfg
        .job
        .as_ref()
        .map_or_else(|| target.job.clone(), |j| j.label.clone());
    Ok(Built {
        bundle,
        hash,
        effective_hash: cfg.hash,
        job_label,
    })
}

/// `POST /api/targets/{id}/releases` (tenant_admin): 201 with a new release, or 200 with the latest
/// one when the build hashes the same. Numbering is serialised per target.
async fn build_release(
    State(st): State<AppState>,
    ctx: TenantCtx,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Release>), ApiError> {
    ctx.require(Role::TenantAdmin)?;
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let target = target_of(&mut tx, &id).await?;
    db::lock_target(&mut tx, target.id).await?;
    let built = build(&mut tx, ctx.tenant, &target).await?;
    if let Some(latest) = db::latest_release(&mut tx, target.id).await?
        && latest.hash == built.hash
    {
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(latest)));
    }
    let release_id = db::next_release_id(&mut tx, target.id).await?;
    let body = release_body(&release_id, &target.job, &built.bundle).map_err(internal)?;
    let release = db::insert_release(
        &mut tx,
        NewRelease {
            tenant: ctx.tenant,
            target: target.id,
            release_id: &release_id,
            hash: &built.hash,
            effective_hash: &built.effective_hash,
            job_label: &built.job_label,
            body: &body,
            by: &ctx.caller.sub,
        },
    )
    .await?;
    audit::event(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "release.build",
        &format!("config_release:{}", release.id),
        &json!({"release_id": release.release_id, "hash": release.hash, "job": target.job}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(release)))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<String>,
    before: Option<String>,
}

/// `GET /api/targets/{id}/releases`: newest first, paged.
async fn list_releases(
    State(st): State<AppState>,
    ctx: TenantCtx,
    Path(id): Path<String>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<Release>>, ApiError> {
    let Query(q) = query?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = TimeCursor::decode(q.before.as_deref())?;
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let target = target_of(&mut tx, &id).await?;
    let rows = db::list_releases(&mut tx, target.id, before, page::fetch(limit)).await?;
    tx.commit().await?;
    Ok(Json(Paged::new(rows, limit, |r| {
        TimeCursor::encode(r.ts_us, r.id)
    })))
}

/// `GET /api/releases/{id}`: the release, its PUT body and its push steps.
async fn show_release(
    State(st): State<AppState>,
    ctx: TenantCtx,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let uuid = Uuid::parse_str(&id).map_err(|_| release_not_found(&id))?;
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let (release, body) = db::get_release(&mut tx, uuid)
        .await?
        .ok_or_else(|| release_not_found(&id))?;
    let steps = db::push_steps(&mut tx, uuid).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"release": release, "body": body, "push_steps": steps}),
    ))
}

fn carebus_unconfigured() -> ApiError {
    ApiError::Unavailable(
        "carebus_unconfigured",
        "care-bus is not configured (CAREBUS_API_URL, CAREBUS_TOKEN_URL, CAREBUS_CLIENT_ID, \
         CAREBUS_CLIENT_SECRET)"
            .into(),
    )
}

/// `POST /api/releases/{id}/push` (tenant_admin): provision the tenant and job into care-bus, then PUT
/// the release in this tenant. Every call is recorded as a push step, failures included, before the
/// answer is given; the care-bus report (with its conflicts) is returned as `carebus`.
async fn push(
    State(st): State<AppState>,
    ctx: TenantCtx,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    ctx.require(Role::TenantAdmin)?;
    let client = st.carebus.clone().ok_or_else(carebus_unconfigured)?;
    let uuid = Uuid::parse_str(&id).map_err(|_| release_not_found(&id))?;
    // Read, then let go of the transaction: no network call inside one.
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let (release, body) = db::get_release(&mut tx, uuid)
        .await?
        .ok_or_else(|| release_not_found(&id))?;
    let target = db::get_target(&mut tx, release.target)
        .await?
        .ok_or_else(|| internal(anyhow::anyhow!("release {uuid} has no target")))?;
    tx.commit().await?;
    let tenant = tenants::get(&st.db, ctx.tenant)
        .await?
        .ok_or_else(|| tenancy::tenant_not_found(ctx.tenant))?;

    let (mut steps, provisioned) = provision::ensure_tenant_and_job(
        &client,
        &st.owner,
        &tenant,
        &target.job,
        &release.job_label,
    )
    .await;
    let mut failure = provisioned.err();
    let mut report = Value::Null;
    if failure.is_none() {
        let r = client
            .put_release(ctx.tenant, &target.job, &release.release_id, &body)
            .await;
        steps.push(StepOutcome::of("release", &r));
        match r {
            Ok(resp) => report = resp.body,
            Err(e) => failure = Some(e),
        }
    }

    let mut tx = st.tx(Some(ctx.tenant)).await?;
    for s in &steps {
        db::insert_step(
            &mut tx,
            NewStep {
                tenant: ctx.tenant,
                release: release.id,
                step: s.step,
                request_hash: (s.step == "release").then_some(release.hash.as_str()),
                status: s.status.map(i32::from),
                ok: s.ok,
                response: Some(&s.body).filter(|b| !b.is_null()),
                error_code: s.error_code.as_deref(),
                by: &ctx.caller.sub,
            },
        )
        .await?;
    }
    let conflicts = report["conflicts"].as_array().map_or(0, Vec::len);
    audit::event(
        &mut tx,
        &ctx.caller.sub,
        Some(ctx.tenant),
        "release.push",
        &format!("config_release:{}", release.id),
        &json!({
            "release_id": release.release_id,
            "hash": release.hash,
            "step": steps.last().map(|s| s.step),
            "status": steps.last().and_then(|s| s.status),
            "ok": failure.is_none(),
            "conflicts": conflicts,
        }),
    )
    .await?;
    let recorded = db::push_steps(&mut tx, release.id).await?;
    tx.commit().await?;

    if let Some(e) = failure {
        let step = steps.last().map_or("tenant", |s| s.step);
        return Err(match e {
            CarebusError::Rejected {
                status: 409,
                code: Some(c),
                ..
            } if step == "release" && c == "release_hash_mismatch" => ApiError::Conflict(
                "carebus_release_conflict",
                format!(
                    "care-bus already holds release {} with another hash",
                    release.release_id
                ),
            ),
            CarebusError::Rejected { status, code, .. } => ApiError::Detailed {
                status: StatusCode::BAD_GATEWAY,
                code: "carebus_rejected",
                message: format!("care-bus refused the {step} step (HTTP {status})"),
                errors: vec![json!({"step": step, "status": status, "code": code})],
            },
            CarebusError::Unavailable(why) => {
                tracing::warn!(release = %release.id, step, %why, "care-bus unavailable");
                ApiError::BadGateway(
                    "carebus_unavailable",
                    format!("care-bus is unavailable (the {step} step)"),
                )
            }
        });
    }
    Ok(Json(
        json!({"release": release, "steps": recorded, "carebus": report}),
    ))
}

/// `GET /api/targets/{id}/drift` (tenant_user): what care-bus holds against what was pushed and what
/// the configuration builds now. `status` is `in_sync`, `drift` (care-bus's current release is not the
/// last one pushed), `never_pushed`, `carebus_has_no_release` or `carebus_unavailable` (an upstream
/// failure is a status here, not a 502). `config_changed`: a fresh build hashes differently from the
/// latest built release, or does not build.
async fn drift(
    State(st): State<AppState>,
    ctx: TenantCtx,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let client = st.carebus.clone().ok_or_else(carebus_unconfigured)?;
    let mut tx = st.tx(Some(ctx.tenant)).await?;
    let target = target_of(&mut tx, &id).await?;
    let latest = db::latest_release(&mut tx, target.id).await?;
    let last_pushed = db::last_pushed(&mut tx, target.id).await?;
    let config_changed = match build(&mut tx, ctx.tenant, &target).await {
        Ok(b) => latest.as_ref().is_none_or(|l| l.hash != b.hash),
        Err(_) => true,
    };
    tx.rollback().await?;

    let current = client.current_release(ctx.tenant, &target.job).await;
    if let Err(e) = &current {
        tracing::warn!(release_target = %target.id, error = %e, "care-bus release read-back failed");
    }
    let status = match (&current, &last_pushed) {
        (Err(_), _) => "carebus_unavailable",
        (Ok(_), None) => "never_pushed",
        (Ok(None), Some(_)) => "carebus_has_no_release",
        (Ok(Some(r)), Some((release_id, hash, _))) => {
            if r["release_id"] == release_id.as_str() && r["hash"] == hash.as_str() {
                "in_sync"
            } else {
                "drift"
            }
        }
    };
    let carebus = match &current {
        Ok(Some(r)) => json!({
            "release_id": r["release_id"],
            "hash": r["hash"],
            "applied_at": r["applied_at"],
            "conflicts": r["conflicts"],
        }),
        _ => Value::Null,
    };
    Ok(Json(json!({
        "status": status,
        "carebus": carebus,
        "last_pushed": last_pushed.map(|(release_id, hash, pushed_at)| {
            json!({"release_id": release_id, "hash": hash, "pushed_at": pushed_at})
        }),
        "latest_built": latest.map(|l| json!({"release_id": l.release_id, "hash": l.hash})),
        "config_changed": config_changed,
    })))
}
