use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;

use crate::auth::Caller;
use crate::db::users;
use crate::error::ApiError;
use crate::license;
use crate::state::AppState;
use crate::tenancy;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/license/status", get(status))
}

/// The licence state of the caller's tenant (the super-admin's: `X-Active-Tenant-Id`, else their
/// own). Exempt from gate 2 and never 402, so a client blocked by the gate can learn why
/// (commonpractice resolves it without the gate for the same reason).
async fn status(
    State(st): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let header = if caller.super_admin {
        tenancy::active_tenant_header(&headers)?
    } else {
        None
    };
    let tenant = match header {
        Some(t) => Some(t),
        None => users::home(&st.db, &caller.sub).await?.map(|(t, _)| t),
    };
    Ok(Json(license::status(&st, tenant).await))
}
