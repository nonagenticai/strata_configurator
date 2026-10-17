use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::Caller;
use crate::db::tenants;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/me", get(me))
}

/// Who the caller is and where they are a member.
async fn me(State(st): State<AppState>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let memberships = tenants::memberships(&st.db, &caller.sub).await?;
    Ok(Json(json!({
        "sub": caller.sub,
        "super_admin": caller.super_admin,
        "memberships": memberships,
    })))
}
