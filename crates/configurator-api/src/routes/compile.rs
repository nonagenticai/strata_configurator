//! Compile one tenant-visible snapshot through the pure compiler.
use crate::{
    db::{axes, dimensions, profiles, rows},
    error::ApiError,
    state::AppState,
    tenancy::Acting,
};
use axum::{
    Json, Router,
    extract::{
        Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    routing::post,
};
use configurator_core::{
    axis::Profile,
    carebus::project_carebus,
    compile::{self, EffectiveConfig, ErrorCode, Known, Selection},
    layer::{LayerKey, Scope},
    overlay::{Kind, Op},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/compile", post(run))
}

/// A selection as callers write it (`POST /api/compile`, a release target's stored selection). The
/// tenant is never part of it: it is the tenant the request acts in.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub industry: Option<String>,
    #[serde(default)]
    pub departments: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    #[serde(default)]
    pub dimensions: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct View {
    view: Option<String>,
}

pub async fn platform_layers(conn: &mut sqlx::PgConnection) -> Result<Vec<LayerKey>, ApiError> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('scope', scope, 'industry', industry, 'department', department, 'job', job, 'rule', rule, 'tenant', tenant, 'dimension', CASE WHEN dimension_key IS NULL THEN NULL ELSE jsonb_build_array(dimension_key, dimension_value) END) FROM layer WHERE tenant IS NULL ORDER BY scope, industry, department, job, rule, dimension_key, dimension_value"
    ).fetch_all(conn).await?;
    rows.into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| ApiError::Internal(e.into())))
        .collect()
}

async fn run(
    State(st): State<AppState>,
    who: Acting,
    query: Result<Query<View>, QueryRejection>,
    body: Result<Json<SelectionRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(view) = query?;
    if view.view.as_deref().is_some_and(|v| v != "carebus") {
        return Err(ApiError::BadRequest(
            "invalid_view",
            "view must be carebus".into(),
        ));
    }
    let Json(b) = body?;
    // One snapshot, in the tenant acted in (RLS) or, for the platform view, on the owner pool.
    let mut tx = st.snapshot(who.tenant()).await?;
    let result = compile_selection(&mut tx, who.tenant(), b).await?;
    tx.commit().await?;
    Ok(Json(if view.view.is_some() {
        json!(project_carebus(&result))
    } else {
        json!(result)
    }))
}

/// Compile `b` in `tenant` (`None`: the platform view) over `tx`. 400 `invalid_selection` when the
/// selection names something unknown, 422 `compile_failed` for row errors, each listed in `errors`.
pub async fn compile_selection(
    tx: &mut sqlx::PgConnection,
    tenant: Option<Uuid>,
    b: SelectionRequest,
) -> Result<EffectiveConfig, ApiError> {
    if b.profile_id.is_some() && b.profile.is_some() {
        return Err(ApiError::BadRequest(
            "invalid_selection",
            "supply profile_id or profile, not both".into(),
        ));
    }
    let profile = if let Some(id) = b.profile_id {
        profiles::get(&mut *tx, tenant, id)
            .await?
            .ok_or_else(|| ApiError::Missing("profile_not_found", "profile does not exist".into()))?
            .values
    } else {
        b.profile.unwrap_or_default()
    };
    let sel = Selection {
        industry: b.industry,
        departments: b.departments,
        job: b.job,
        profile,
        dimensions: b.dimensions,
        tenant,
    };
    let layers = platform_layers(&mut *tx).await?;
    // Every platform row once: base-layer jobs are the selectable jobs, and every platform `add` is
    // an inherited object a tenant tombstone may name outside this selection.
    let platform_rows = rows::load_for_layers_conn(&mut *tx, &layers).await?;
    let jobs = platform_rows
        .iter()
        .filter(|r| r.layer.scope == Scope::Base && r.kind == Kind::Job && r.op == Op::Add)
        .map(|r| r.key.clone())
        .collect();
    let inherited = platform_rows
        .iter()
        .filter(|r| r.op == Op::Add)
        .map(|r| compile::inherited_key(r.kind, &r.key))
        .collect();
    let mut known_dimensions: BTreeMap<String, BTreeSet<String>> = dimensions::load(&mut *tx)
        .await?
        .into_iter()
        .map(|d| (d.key, BTreeSet::new()))
        .collect();
    for v in dimensions::values(&mut *tx, tenant).await? {
        known_dimensions
            .entry(v.dimension)
            .or_default()
            .insert(v.key);
    }
    let known = Known {
        industries: layers
            .iter()
            .filter(|l| matches!(l.scope, Scope::Industry | Scope::IndustryDept))
            .filter_map(|l| l.industry.clone())
            .collect(),
        departments: known_dimensions
            .get("department")
            .cloned()
            .unwrap_or_default(),
        jobs,
        dimensions: known_dimensions,
        axes: axes::load(&mut *tx).await?,
        inherited,
    };
    let rules = axes::rules(&mut *tx).await?;
    let rows = rows::load_for_layers_conn(&mut *tx, &compile::layer_order(&sel, &rules)).await?;
    compile::compile(&sel, &known, &rules, &rows).map_err(|errors| {
        let invalid = errors.iter().all(|e| e.code == ErrorCode::InvalidSelection);
        ApiError::Detailed {
            status: if invalid {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            },
            code: if invalid {
                "invalid_selection"
            } else {
                "compile_failed"
            },
            message: format!("{} compile error(s); see errors", errors.len()),
            errors: errors.iter().map(|e| json!(e)).collect(),
        }
    })
}
