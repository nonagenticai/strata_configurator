//! Layers and their overlay rows, with revision history (spec sections 4.1 and 10.1).
//!
//! Visibility: a tenant caller sees every platform layer and its own tenant's layer; the super-admin
//! in the platform view sees platform layers only. Another tenant's layer is 404 on every route, the
//! same as a layer that does not exist. Reads need any tenant role (`tenant_user`); writes to a tenant
//! layer need `tenant_admin`, writes to a platform layer need the super-admin. Every write is audited
//! in its transaction.
//!
//! Database access follows the data: a tenant's layer through the runtime pool with that tenant set
//! (RLS), platform layers through the owner pool ([`AppState::tx`]). The SQL tenant predicates stay
//! as defence in depth.
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use configurator_core::layer::{LayerKey, Scope};
use configurator_core::overlay::{FieldError, Kind, Op, validate_fields, validate_key};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::audit;
use crate::auth::require_super_admin;
use crate::db::layers::{self, Filter, Layer};
use crate::db::rows::{self, NewRev, Row};
use crate::error::ApiError;
use crate::page::{self, Paged, TimeCursor};
use crate::seeder::{kind_name, scope_name};
use crate::state::AppState;
use crate::tenancy::{Acting, Role, tenant_required};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/layers", get(list).post(create))
        .route("/api/layers/{id}", get(show))
        .route("/api/layers/{id}/rows", get(list_rows).post(add_row))
        .route(
            "/api/layers/{id}/rows/{kind}/{key}",
            put(revise_row).delete(retire_row),
        )
        .route(
            "/api/layers/{id}/rows/{kind}/{key}/revisions",
            get(revisions),
        )
}

// --- errors -------------------------------------------------------------------------------------

fn layer_not_found(id: &str) -> ApiError {
    ApiError::Missing("layer_not_found", format!("layer {id} does not exist"))
}

fn row_not_found(kind: Kind, key: &str) -> ApiError {
    ApiError::Missing(
        "row_not_found",
        format!("this layer has no {} {key:?}", kind_name(kind)),
    )
}

fn revision_conflict(kind: Kind, key: &str) -> ApiError {
    ApiError::Conflict(
        "revision_conflict",
        format!(
            "{} {key:?} changed since expected_rev; reload it and retry",
            kind_name(kind)
        ),
    )
}

fn row_exists(kind: Kind, key: &str) -> ApiError {
    ApiError::Conflict(
        "row_exists",
        format!(
            "this layer already has a current {} {key:?}; PUT a new revision instead",
            kind_name(kind)
        ),
    )
}

/// A snake_case path or query value as the enum it names; otherwise 400 `code`.
fn parse_enum<T: serde::de::DeserializeOwned>(
    code: &'static str,
    what: &str,
    s: &str,
) -> Result<T, ApiError> {
    serde_json::from_value(Value::String(s.to_owned()))
        .map_err(|_| ApiError::BadRequest(code, format!("{s:?} is not a valid {what}")))
}

fn parse_kind(s: &str) -> Result<Kind, ApiError> {
    parse_enum(
        "invalid_kind",
        "kind (entity | attribute | relation | job)",
        s,
    )
}

/// 400 `invalid_key`, with the key error in `errors`.
fn check_key(kind: Kind, key: &str) -> Result<(), ApiError> {
    validate_key(kind, key).map_err(|e| ApiError::Detailed {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_key",
        message: e.message.clone(),
        errors: vec![json!(e)],
    })
}

/// 400 `invalid_fields`, listing every field error.
fn check_fields(kind: Kind, op: Op, fields: &Map<String, Value>) -> Result<(), ApiError> {
    validate_fields(kind, op, fields).map_err(|errs: Vec<FieldError>| ApiError::Detailed {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_fields",
        message: format!("{} field error(s); see errors", errs.len()),
        errors: errs.iter().map(|e| json!(e)).collect(),
    })
}

// --- access -------------------------------------------------------------------------------------

/// The layer `id` if `who` may see it; anything else (unknown id, malformed id, another tenant's layer,
/// a tenant layer in the platform view) is the same 404 `layer_not_found`.
async fn visible(st: &AppState, who: &Acting, id: &str) -> Result<Layer, ApiError> {
    let Ok(uuid) = Uuid::parse_str(id) else {
        return Err(layer_not_found(id));
    };
    let mut tx = st.tx(who.tenant()).await?;
    match layers::get(&mut tx, uuid).await? {
        Some(l) if l.key.tenant.is_none() || l.key.tenant == who.tenant() => Ok(l),
        _ => Err(layer_not_found(id)),
    }
}

/// Platform layers are the super-admin's; a tenant layer is its tenant's `tenant_admin`s'.
fn may_write(who: &Acting, l: &Layer) -> Result<(), ApiError> {
    match (l.key.tenant, who) {
        (None, _) => require_super_admin(who.caller()),
        (Some(_), Acting::Tenant(ctx)) => ctx.require(Role::TenantAdmin),
        // `visible` never returns a tenant layer in the platform view.
        (Some(_), Acting::Platform(_)) => Err(layer_not_found(&l.id.to_string())),
    }
}

fn object(l: &Layer, kind: Kind, key: &str) -> String {
    format!("overlay_row:{}:{}:{key}", l.id, kind_name(kind))
}

// --- layers -------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct LayerQuery {
    scope: Option<String>,
    industry: Option<String>,
    job: Option<String>,
    limit: Option<String>,
    before: Option<String>,
}

/// `GET /api/layers?scope=&industry=&job=`: the visible layers, newest first, paged.
async fn list(
    State(st): State<AppState>,
    who: Acting,
    q: Result<Query<LayerQuery>, QueryRejection>,
) -> Result<Json<Paged<Layer>>, ApiError> {
    let Query(q) = q?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = TimeCursor::decode(q.before.as_deref())?;
    let scope = q
        .scope
        .as_deref()
        .map(|s| {
            parse_enum::<Scope>(
                "invalid_scope",
                "scope (base | profile | industry | industry_dept | job | tenant)",
                s,
            )
        })
        .transpose()?
        .map(scope_name);
    let f = Filter {
        scope,
        industry: q.industry,
        job: q.job,
    };
    let mut tx = st.tx(who.tenant()).await?;
    let items = layers::list(&mut tx, who.tenant(), &f, before, page::fetch(limit)).await?;
    Ok(Json(Paged::new(items, limit, |l| {
        TimeCursor::encode(l.ts_us, l.id)
    })))
}

async fn show(
    State(st): State<AppState>,
    who: Acting,
    Path(id): Path<String>,
) -> Result<Json<Layer>, ApiError> {
    Ok(Json(visible(&st, &who, &id).await?))
}

/// A layer key without `tenant`: a `tenant` layer is always the tenant the caller acts in.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewLayer {
    scope: Scope,
    industry: Option<String>,
    department: Option<String>,
    job: Option<String>,
    rule: Option<String>,
    dimension: Option<(String, String)>,
}

/// `POST /api/layers`: scope `tenant` needs `tenant_admin` in the tenant acted in; any other scope needs
/// the super-admin. 400 `invalid_layer` for a key that breaks the spec 4.1 table; 409 `layer_exists`.
async fn create(
    State(st): State<AppState>,
    who: Acting,
    body: Result<Json<NewLayer>, JsonRejection>,
) -> Result<(StatusCode, Json<Layer>), ApiError> {
    let Json(b) = body?;
    let tenant = if b.scope == Scope::Tenant {
        match &who {
            Acting::Tenant(ctx) => {
                ctx.require(Role::TenantAdmin)?;
                Some(ctx.tenant)
            }
            Acting::Platform(_) => return Err(tenant_required()),
        }
    } else {
        require_super_admin(who.caller())?;
        None
    };
    let key = LayerKey {
        scope: b.scope,
        industry: b.industry,
        department: b.department,
        job: b.job,
        rule: b.rule,
        dimension: b.dimension,
        tenant,
    };
    key.validate()
        .map_err(|e| ApiError::BadRequest("invalid_layer", e.to_string()))?;
    let mut tx = st.tx(key.tenant).await?;
    let Some(l) = layers::create(&mut tx, &key).await? else {
        return Err(ApiError::Conflict(
            "layer_exists",
            "a layer with this key already exists".into(),
        ));
    };
    audit::record(
        &mut tx,
        &who.caller().sub,
        l.key.tenant,
        "layer.create",
        &format!("layer:{}", l.id),
        None,
        Some(&json!(l)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(l)))
}

// --- rows ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RowsQuery {
    kind: Option<String>,
    limit: Option<String>,
    before: Option<String>,
}

/// `GET /api/layers/{id}/rows?kind=`: the layer's current rows, newest first, paged.
async fn list_rows(
    State(st): State<AppState>,
    who: Acting,
    Path(id): Path<String>,
    q: Result<Query<RowsQuery>, QueryRejection>,
) -> Result<Json<Paged<Row>>, ApiError> {
    let l = visible(&st, &who, &id).await?;
    let Query(q) = q?;
    let kind = q.kind.as_deref().map(parse_kind).transpose()?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = TimeCursor::decode(q.before.as_deref())?;
    let mut tx = st.tx(who.tenant()).await?;
    let items = rows::list_current(&mut tx, l.id, kind, before, page::fetch(limit)).await?;
    Ok(Json(Paged::new(items, limit, |r| {
        TimeCursor::encode(r.ts_us, r.id)
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewRow {
    kind: Kind,
    key: String,
    op: Op,
    #[serde(default)]
    fields: Map<String, Value>,
}

/// `POST /api/layers/{id}/rows`: a new object in this layer → 201 with its revision (1, or one past the
/// newest retired revision when the object was retired before). 409 `row_exists` when it is current.
async fn add_row(
    State(st): State<AppState>,
    who: Acting,
    Path(id): Path<String>,
    body: Result<Json<NewRow>, JsonRejection>,
) -> Result<(StatusCode, Json<Row>), ApiError> {
    let l = visible(&st, &who, &id).await?;
    may_write(&who, &l)?;
    let Json(b) = body?;
    check_key(b.kind, &b.key)?;
    check_fields(b.kind, b.op, &b.fields)?;
    let mut tx = st.tx(l.key.tenant).await?;
    let latest = rows::latest(&mut tx, l.id, b.kind, &b.key).await?;
    if latest.as_ref().is_some_and(|r| r.is_current) {
        return Err(row_exists(b.kind, &b.key));
    }
    let new = NewRev {
        layer_id: l.id,
        kind: b.kind,
        key: &b.key,
        op: b.op,
        fields: &b.fields,
        rev: latest.map_or(1, |r| r.rev + 1),
        author: &who.caller().sub,
    };
    let row = match rows::insert(&mut tx, new).await {
        Ok(r) => r,
        Err(e) if rows::is_conflict(&e) => return Err(row_exists(b.kind, &b.key)),
        Err(e) => return Err(e.into()),
    };
    audit::record(
        &mut tx,
        &who.caller().sub,
        l.key.tenant,
        "overlay_row.create",
        &object(&l, b.kind, &b.key),
        None,
        Some(&json!(row)),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    op: Op,
    #[serde(default)]
    fields: Map<String, Value>,
    expected_rev: i32,
}

/// After a retire that matched nothing: 404 when the object never existed here, else 409.
async fn no_match(
    tx: &mut sqlx::PgConnection,
    l: &Layer,
    kind: Kind,
    key: &str,
) -> Result<ApiError, ApiError> {
    Ok(match rows::latest(tx, l.id, kind, key).await? {
        None => row_not_found(kind, key),
        Some(_) => revision_conflict(kind, key),
    })
}

/// `PUT /api/layers/{id}/rows/{kind}/{key}`: a new current revision, `expected_rev + 1`, retiring
/// `expected_rev` in the same transaction. 409 `revision_conflict` when `expected_rev` is not the current
/// revision, including when a concurrent writer got there first.
async fn revise_row(
    State(st): State<AppState>,
    who: Acting,
    Path((id, kind, key)): Path<(String, String, String)>,
    body: Result<Json<Revision>, JsonRejection>,
) -> Result<Json<Row>, ApiError> {
    let l = visible(&st, &who, &id).await?;
    may_write(&who, &l)?;
    let kind = parse_kind(&kind)?;
    check_key(kind, &key)?;
    let Json(b) = body?;
    check_fields(kind, b.op, &b.fields)?;
    let mut tx = st.tx(l.key.tenant).await?;
    let Some(prev) = rows::retire(&mut tx, l.id, kind, &key, b.expected_rev).await? else {
        return Err(no_match(&mut tx, &l, kind, &key).await?);
    };
    let new = NewRev {
        layer_id: l.id,
        kind,
        key: &key,
        op: b.op,
        fields: &b.fields,
        rev: prev.rev + 1,
        author: &who.caller().sub,
    };
    let row = match rows::insert(&mut tx, new).await {
        Ok(r) => r,
        Err(e) if rows::is_conflict(&e) => return Err(revision_conflict(kind, &key)),
        Err(e) => return Err(e.into()),
    };
    audit::record(
        &mut tx,
        &who.caller().sub,
        l.key.tenant,
        "overlay_row.update",
        &object(&l, kind, &key),
        Some(&json!(prev)),
        Some(&json!(row)),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(row))
}

#[derive(Deserialize)]
struct RetireQuery {
    expected_rev: Option<i32>,
}

/// `DELETE /api/layers/{id}/rows/{kind}/{key}?expected_rev=`: retire the current revision (it stays as
/// history; the layer no longer has the object). 204.
async fn retire_row(
    State(st): State<AppState>,
    who: Acting,
    Path((id, kind, key)): Path<(String, String, String)>,
    q: Result<Query<RetireQuery>, QueryRejection>,
) -> Result<StatusCode, ApiError> {
    let l = visible(&st, &who, &id).await?;
    may_write(&who, &l)?;
    let kind = parse_kind(&kind)?;
    let Query(q) = q?;
    let Some(expected_rev) = q.expected_rev else {
        return Err(ApiError::BadRequest(
            "invalid_query",
            "expected_rev is required".into(),
        ));
    };
    let mut tx = st.tx(l.key.tenant).await?;
    let Some(prev) = rows::retire(&mut tx, l.id, kind, &key, expected_rev).await? else {
        return Err(no_match(&mut tx, &l, kind, &key).await?);
    };
    audit::record(
        &mut tx,
        &who.caller().sub,
        l.key.tenant,
        "overlay_row.retire",
        &object(&l, kind, &key),
        Some(&json!(prev)),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RevisionsQuery {
    limit: Option<String>,
    before: Option<String>,
}

/// `GET …/{kind}/{key}/revisions`: every revision, newest first, paged (`before` = a revision number).
async fn revisions(
    State(st): State<AppState>,
    who: Acting,
    Path((id, kind, key)): Path<(String, String, String)>,
    q: Result<Query<RevisionsQuery>, QueryRejection>,
) -> Result<Json<Paged<Row>>, ApiError> {
    let l = visible(&st, &who, &id).await?;
    let kind = parse_kind(&kind)?;
    let Query(q) = q?;
    let limit = page::limit(q.limit.as_deref())?;
    let before = page::rev_cursor(q.before.as_deref())?;
    let mut tx = st.tx(who.tenant()).await?;
    let items = rows::revisions(&mut tx, l.id, kind, &key, before, page::fetch(limit)).await?;
    if items.is_empty() && before.is_none() {
        return Err(row_not_found(kind, &key));
    }
    Ok(Json(Paged::new(items, limit, |r| r.rev.to_string())))
}
