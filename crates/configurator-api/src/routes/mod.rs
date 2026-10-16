use axum::Router;

use crate::error;
use crate::state::AppState;

pub mod health;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::routes())
        .fallback(error::fallback)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state)
}
