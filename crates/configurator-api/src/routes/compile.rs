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
    compile::{self, ErrorCode, Known, Selection},
    layer::{LayerKey, Scope},
    overlay::{Kind, Op},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/compile", post(run))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    industry: Option<String>,
    #[serde(default)]
    departments: BTreeSet<String>,
    job: Option<String>,
    profile_id: Option<Uuid>,
    profile: Option<Profile>,
    #[serde(default)]
    dimensions: BTreeMap<String, String>,
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
    body: Result<Json<Request>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(view) = query?;
    if view.view.as_deref().is_some_and(|v| v != "carebus") {
        return Err(ApiError::BadRequest(
            "invalid_view",
            "view must be carebus".into(),
        ));
    }
    let Json(b) = body?;
    if b.profile_id.is_some() && b.profile.is_some() {
        return Err(ApiError::BadRequest(
            "invalid_selection",
            "supply profile_id or profile, not both".into(),
        ));
    }
    let mut tx = st.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let profile = if let Some(id) = b.profile_id {
        profiles::get(&mut tx, who.tenant(), id)
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
        tenant: who.tenant(),
    };
    let layers = platform_layers(&mut tx).await?;
    let base: Vec<_> = layers
        .iter()
        .filter(|l| l.scope == Scope::Base)
        .cloned()
        .collect();
    let jobs = rows::load_for_layers_conn(&mut tx, &base)
        .await?
        .into_iter()
        .filter(|r| r.kind == Kind::Job && r.op == Op::Add)
        .map(|r| r.key)
        .collect();
    let mut known_dimensions: BTreeMap<String, BTreeSet<String>> = dimensions::load(&mut tx)
        .await?
        .into_iter()
        .map(|d| (d.key, BTreeSet::new()))
        .collect();
    for v in dimensions::values(&mut tx, who.tenant()).await? {
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
        axes: axes::load(&mut tx).await?,
    };
    let rules = axes::rules(&mut tx).await?;
    let rows = rows::load_for_layers_conn(&mut tx, &compile::layer_order(&sel, &rules)).await?;
    let result = compile::compile(&sel, &known, &rules, &rows).map_err(|errors| {
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
    })?;
    tx.commit().await?;
    Ok(Json(if view.view.is_some() {
        json!(project_carebus(&result))
    } else {
        json!(result)
    }))
}
