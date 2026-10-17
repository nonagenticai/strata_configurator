//! Catalog reads and transactional, audited writes (plan Task 10).
//!
//! Reads run in the tenant acted in (runtime pool, RLS) or, in the platform view, on the owner pool
//! ([`AppState::tx`]). Platform writes (axes, values, rules, dimensions, platform profiles and
//! dimension values) are the super-admin's and run on the owner pool; tenant writes run under RLS.
use crate::{
    audit,
    auth::require_super_admin,
    db::{axes, dimensions, layers, profiles},
    error::ApiError,
    page::{self, Paged},
    state::AppState,
    tenancy::{Acting, Role},
};
use axum::extract::{
    Path, Query, State,
    rejection::{JsonRejection, QueryRejection},
};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use configurator_core::axis::{Axis, AxisValue, Cardinality, Condition, Profile, validate_axis};
use configurator_core::layer::{LayerKey, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/axes", get(list_axes).post(create_axis))
        .route(
            "/api/axes/{key}/values",
            get(list_axis_values).post(create_axis_value),
        )
        .route("/api/axis-rules", get(list_rules).post(create_rule))
        .route(
            "/api/dimensions",
            get(list_dimensions).post(create_dimension),
        )
        .route(
            "/api/dimensions/{key}/values",
            get(list_dimension_values).post(create_dimension_value),
        )
        .route("/api/profiles", get(list_profiles).post(create_profile))
        .route("/api/profiles/{id}", get(show_profile).put(update_profile))
}

#[derive(Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub before: Option<String>,
}

pub fn key_page<T>(
    rows: Vec<T>,
    q: &ListQuery,
    key: impl Fn(&T) -> String,
) -> Result<Paged<T>, ApiError> {
    let limit = page::limit(q.limit.as_deref())?;
    let rows = rows
        .into_iter()
        .filter(|r| q.before.as_ref().is_none_or(|b| key(r) > *b))
        .take(page::fetch(limit) as usize)
        .collect();
    Ok(Paged::new(rows, limit, key))
}

fn nonempty(code: &'static str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > 256 {
        Err(ApiError::BadRequest(
            code,
            "key and label must contain 1–256 characters".into(),
        ))
    } else {
        Ok(())
    }
}

fn write_error(e: sqlx::Error, code: &'static str) -> ApiError {
    if matches!(&e, sqlx::Error::Database(d) if d.is_unique_violation()) {
        ApiError::Conflict(code, "an item with this key already exists".into())
    } else {
        e.into()
    }
}

fn invalid_axis(e: impl std::fmt::Display) -> ApiError {
    ApiError::BadRequest("invalid_axis", e.to_string())
}

async fn list_axes(
    State(st): State<AppState>,
    who: Acting,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<Axis>>, ApiError> {
    let Query(q) = q?;
    let rows = axes::load(&mut *st.tx(who.tenant()).await?).await?;
    Ok(Json(key_page(rows, &q, |a| a.key.clone())?))
}

async fn list_axis_values(
    State(st): State<AppState>,
    who: Acting,
    Path(key): Path<String>,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<AxisValue>>, ApiError> {
    let Query(q) = q?;
    let axis = axes::load(&mut *st.tx(who.tenant()).await?)
        .await?
        .into_iter()
        .find(|a| a.key == key)
        .ok_or_else(|| ApiError::Missing("axis_not_found", key))?;
    Ok(Json(key_page(axis.values, &q, |v| v.key.clone())?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAxis {
    key: String,
    cardinality: Cardinality,
    label: Option<String>,
    description: Option<String>,
}

async fn create_axis(
    State(st): State<AppState>,
    who: Acting,
    b: Result<Json<NewAxis>, JsonRejection>,
) -> Result<(StatusCode, Json<Axis>), ApiError> {
    require_super_admin(who.caller())?;
    let Json(b) = b?;
    nonempty("invalid_axis", &b.key)?;
    if let Some(label) = &b.label {
        nonempty("invalid_axis", label)?;
    }
    if b.description
        .as_deref()
        .is_some_and(|s| s.trim().is_empty())
    {
        return Err(invalid_axis("description must not be blank"));
    }
    let a = Axis {
        key: b.key,
        cardinality: b.cardinality,
        values: vec![],
        label: b.label,
        description: b.description,
    };
    let mut tx = st.owner.begin().await?;
    sqlx::query("INSERT INTO axis (key, cardinality, label, description) VALUES ($1, $2, $3, $4)")
        .bind(&a.key)
        .bind(match a.cardinality {
            Cardinality::One => "one",
            Cardinality::Many => "many",
        })
        .bind(&a.label)
        .bind(&a.description)
        .execute(&mut *tx)
        .await
        .map_err(|e| write_error(e, "axis_exists"))?;
    audit::record(
        &mut tx,
        &who.caller().sub,
        None,
        "axis.create",
        &a.key,
        None,
        Some(&json!(a)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(a)))
}

async fn create_axis_value(
    State(st): State<AppState>,
    who: Acting,
    Path(key): Path<String>,
    b: Result<Json<AxisValue>, JsonRejection>,
) -> Result<(StatusCode, Json<AxisValue>), ApiError> {
    require_super_admin(who.caller())?;
    let Json(mut v) = b?;
    if v.description
        .as_deref()
        .is_some_and(|s| s.trim().is_empty())
    {
        return Err(invalid_axis("description must not be blank"));
    }
    nonempty("invalid_axis", &v.key)?;
    if v.label.is_none() {
        v.label = Some(v.key.rsplit('.').next().unwrap_or(&v.key).to_owned());
    }
    nonempty("invalid_axis", v.label.as_deref().unwrap_or_default())?;
    let mut tx = st.owner.begin().await?;
    let mut axis = axes::load(&mut tx)
        .await?
        .into_iter()
        .find(|a| a.key == key)
        .ok_or_else(|| ApiError::Missing("axis_not_found", key.clone()))?;
    axis.values.push(v.clone());
    validate_axis(&axis).map_err(invalid_axis)?;
    sqlx::query("INSERT INTO axis_value (axis, key, parent, label, description) VALUES ($1, $2, $3, $4, $5)")
        .bind(&key).bind(&v.key).bind(&v.parent).bind(&v.label).bind(&v.description).execute(&mut *tx).await.map_err(|e| write_error(e, "axis_value_exists"))?;
    audit::record(
        &mut tx,
        &who.caller().sub,
        None,
        "axis_value.create",
        &format!("{key}:{}", v.key),
        None,
        Some(&json!(v)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Serialize, sqlx::FromRow)]
struct Rule {
    key: String,
    priority: i32,
    condition: Value,
    layer_id: Uuid,
}

async fn list_rules(
    State(st): State<AppState>,
    who: Acting,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<Rule>>, ApiError> {
    let Query(q) = q?;
    let mut tx = st.tx(who.tenant()).await?;
    let rows =
        sqlx::query_as("SELECT key, priority, condition, layer_id FROM axis_rule ORDER BY key")
            .fetch_all(&mut *tx)
            .await?;
    Ok(Json(key_page(rows, &q, |r: &Rule| r.key.clone())?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewRule {
    key: String,
    priority: i32,
    condition: Condition,
}

async fn create_rule(
    State(st): State<AppState>,
    who: Acting,
    b: Result<Json<NewRule>, JsonRejection>,
) -> Result<(StatusCode, Json<Rule>), ApiError> {
    require_super_admin(who.caller())?;
    let Json(b) = b?;
    nonempty("invalid_condition", &b.key)?;
    let mut tx = st.owner.begin().await?;
    b.condition
        .validate(&axes::load(&mut tx).await?)
        .map_err(|e| ApiError::BadRequest("invalid_condition", e.to_string()))?;
    let key = LayerKey {
        scope: Scope::Profile,
        rule: Some(b.key.clone()),
        industry: None,
        department: None,
        job: None,
        dimension: None,
        tenant: None,
    };
    let layer = match layers::create(&mut tx, &key).await? {
        Some(l) => l.id,
        None => {
            sqlx::query_scalar("SELECT id FROM layer WHERE scope = 'profile' AND rule = $1")
                .bind(&b.key)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    let r = Rule {
        key: b.key,
        priority: b.priority,
        condition: json!(b.condition),
        layer_id: layer,
    };
    sqlx::query(
        "INSERT INTO axis_rule (key, priority, condition, layer_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(&r.key)
    .bind(r.priority)
    .bind(&r.condition)
    .bind(r.layer_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| write_error(e, "rule_exists"))?;
    audit::record(
        &mut tx,
        &who.caller().sub,
        None,
        "axis_rule.create",
        &r.key,
        None,
        Some(&json!(r)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(r)))
}

async fn list_dimensions(
    State(st): State<AppState>,
    who: Acting,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<dimensions::Dimension>>, ApiError> {
    let Query(q) = q?;
    let rows = dimensions::load(&mut *st.tx(who.tenant()).await?).await?;
    Ok(Json(key_page(rows, &q, |d| d.key.clone())?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    key: String,
    label: String,
}

async fn create_dimension(
    State(st): State<AppState>,
    who: Acting,
    b: Result<Json<Named>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_super_admin(who.caller())?;
    let Json(b) = b?;
    nonempty("invalid_dimension", &b.key)?;
    nonempty("invalid_dimension", &b.label)?;
    let mut tx = st.owner.begin().await?;
    sqlx::query("INSERT INTO dimension (key, label) VALUES ($1, $2)")
        .bind(&b.key)
        .bind(&b.label)
        .execute(&mut *tx)
        .await
        .map_err(|e| write_error(e, "dimension_exists"))?;
    let out = json!({"key": b.key, "label": b.label});
    audit::record(
        &mut tx,
        &who.caller().sub,
        None,
        "dimension.create",
        &b.key,
        None,
        Some(&out),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)))
}

async fn list_dimension_values(
    State(st): State<AppState>,
    who: Acting,
    Path(key): Path<String>,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<dimensions::DimensionValue>>, ApiError> {
    let Query(q) = q?;
    let mut conn = st.tx(who.tenant()).await?;
    if !dimensions::load(&mut conn)
        .await?
        .iter()
        .any(|d| d.key == key)
    {
        return Err(ApiError::Missing("dimension_not_found", key));
    }
    let rows = dimensions::values(&mut conn, who.tenant())
        .await?
        .into_iter()
        .filter(|v| v.dimension == key)
        .collect();
    Ok(Json(key_page(rows, &q, |v| format!("{}:{}", v.key, v.id))?))
}

async fn create_dimension_value(
    State(st): State<AppState>,
    who: Acting,
    Path(key): Path<String>,
    b: Result<Json<Named>, JsonRejection>,
) -> Result<(StatusCode, Json<dimensions::DimensionValue>), ApiError> {
    // Platform values (no tenant in scope) are the super-admin's: only the super-admin acts with
    // no tenant. Tenant values need `tenant_admin`.
    match &who {
        Acting::Tenant(ctx) => ctx.require(Role::TenantAdmin)?,
        Acting::Platform(c) => require_super_admin(c)?,
    }
    let Json(b) = b?;
    nonempty("invalid_dimension", &b.key)?;
    nonempty("invalid_dimension", &b.label)?;
    let mut tx = st.tx(who.tenant()).await?;
    if !dimensions::load(&mut tx)
        .await?
        .iter()
        .any(|d| d.key == key)
    {
        return Err(ApiError::Missing("dimension_not_found", key));
    }
    let v: dimensions::DimensionValue = sqlx::query_as("INSERT INTO dimension_value (dimension, key, label, tenant) VALUES ($1, $2, $3, $4) RETURNING id, dimension, key, label, tenant")
        .bind(&key).bind(&b.key).bind(&b.label).bind(who.tenant()).fetch_one(&mut *tx).await.map_err(|e| write_error(e, "dimension_value_exists"))?;
    audit::record(
        &mut tx,
        &who.caller().sub,
        who.tenant(),
        "dimension_value.create",
        &v.id.to_string(),
        None,
        Some(&json!(v)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn list_profiles(
    State(st): State<AppState>,
    who: Acting,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Paged<profiles::SavedProfile>>, ApiError> {
    let Query(q) = q?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = q
        .before
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| ApiError::BadRequest("invalid_cursor", "expected profile cursor".into()))?;
    let rows = profiles::load(&mut *st.tx(who.tenant()).await?, who.tenant())
        .await?
        .into_iter()
        .filter(|r| before.is_none_or(|b| r.id < b))
        .take(page::fetch(limit) as usize)
        .collect();
    Ok(Json(Paged::new(rows, limit, |p| p.id.to_string())))
}

async fn visible_profile(
    st: &AppState,
    who: &Acting,
    id: &str,
) -> Result<profiles::SavedProfile, ApiError> {
    let missing = || ApiError::Missing("profile_not_found", "profile does not exist".into());
    let id = Uuid::parse_str(id).map_err(|_| missing())?;
    profiles::get(&mut *st.tx(who.tenant()).await?, who.tenant(), id)
        .await?
        .ok_or_else(missing)
}

async fn show_profile(
    State(st): State<AppState>,
    who: Acting,
    Path(id): Path<String>,
) -> Result<Json<profiles::SavedProfile>, ApiError> {
    Ok(Json(visible_profile(&st, &who, &id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewProfile {
    name: String,
    values: Profile,
}

fn may_write_profile(who: &Acting, tenant: Option<Uuid>) -> Result<(), ApiError> {
    if tenant.is_none() {
        require_super_admin(who.caller())
    } else if let Acting::Tenant(ctx) = who {
        ctx.require(Role::TenantAdmin)
    } else {
        Err(ApiError::Forbidden(Role::TenantAdmin.as_str()))
    }
}

async fn create_profile(
    State(st): State<AppState>,
    who: Acting,
    b: Result<Json<NewProfile>, JsonRejection>,
) -> Result<(StatusCode, Json<profiles::SavedProfile>), ApiError> {
    may_write_profile(&who, who.tenant())?;
    let Json(b) = b?;
    nonempty("invalid_profile", &b.name)?;
    let mut tx = st.tx(who.tenant()).await?;
    b.values
        .validate(&axes::load(&mut tx).await?)
        .map_err(|e| ApiError::BadRequest("invalid_profile", e.to_string()))?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO profile (tenant, name, \"values\") VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(who.tenant())
    .bind(&b.name)
    .bind(json!(b.values))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| write_error(e, "profile_exists"))?;
    let p = profiles::SavedProfile {
        id,
        tenant: who.tenant(),
        name: b.name,
        values: b.values,
    };
    audit::record(
        &mut tx,
        &who.caller().sub,
        p.tenant,
        "profile.create",
        &id.to_string(),
        None,
        Some(&json!(p)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(p)))
}

async fn update_profile(
    State(st): State<AppState>,
    who: Acting,
    Path(id): Path<String>,
    b: Result<Json<NewProfile>, JsonRejection>,
) -> Result<Json<profiles::SavedProfile>, ApiError> {
    let old = visible_profile(&st, &who, &id).await?;
    may_write_profile(&who, old.tenant)?;
    let Json(b) = b?;
    nonempty("invalid_profile", &b.name)?;
    let mut tx = st.tx(old.tenant).await?;
    // Lock before reading the audit's before-image so concurrent updates have an accurate chain.
    sqlx::query("SELECT id FROM profile WHERE id = $1 FOR UPDATE")
        .bind(old.id)
        .execute(&mut *tx)
        .await?;
    let old = profiles::get(&mut tx, who.tenant(), old.id)
        .await?
        .ok_or_else(|| ApiError::Missing("profile_not_found", id))?;
    b.values
        .validate(&axes::load(&mut tx).await?)
        .map_err(|e| ApiError::BadRequest("invalid_profile", e.to_string()))?;
    sqlx::query("UPDATE profile SET name = $2, \"values\" = $3 WHERE id = $1")
        .bind(old.id)
        .bind(&b.name)
        .bind(json!(b.values))
        .execute(&mut *tx)
        .await
        .map_err(|e| write_error(e, "profile_exists"))?;
    let p = profiles::SavedProfile {
        id: old.id,
        tenant: old.tenant,
        name: b.name,
        values: b.values,
    };
    audit::record(
        &mut tx,
        &who.caller().sub,
        p.tenant,
        "profile.update",
        &p.id.to_string(),
        Some(&json!(old)),
        Some(&json!(p)),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(p))
}
