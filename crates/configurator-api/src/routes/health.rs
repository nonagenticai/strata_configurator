use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::db;
use crate::error::ApiError;
use crate::state::{AppState, SeedStatus};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
}

/// Liveness: the process answers. Never touches the database.
async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

/// Readiness: the seeds applied cleanly and the database answers.
async fn ready(State(s): State<AppState>) -> Response {
    if let SeedStatus::Conflict { version, key } = s.seed_status() {
        return ApiError::Unavailable(
            "seed_conflict",
            format!("seed v{version} would add {key}, which already exists"),
        )
        .into_response();
    }
    if !db::ping(&s.db).await {
        return ApiError::Unavailable("db_unavailable", "the database does not answer".into())
            .into_response();
    }
    Json(json!({"status": "ready"})).into_response()
}
