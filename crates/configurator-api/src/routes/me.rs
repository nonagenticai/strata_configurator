use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::Caller;
use crate::db::users;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/me", get(me))
}

/// Who the caller is and their own workspace (`null` when they have none or it is disabled).
async fn me(State(st): State<AppState>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let tenant = users::home_tenant(&st.db, &caller.sub).await?;
    Ok(Json(json!({
        "sub": caller.sub,
        "super_admin": caller.super_admin,
        "tenant": tenant,
    })))
}
