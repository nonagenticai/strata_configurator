use axum::Router;

use crate::error;
use crate::state::AppState;

pub mod catalog;
pub mod compile;
pub mod health;
pub mod layers;
pub mod me;
pub mod platform;
pub mod tenants;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::routes())
        .merge(catalog::routes())
        .merge(compile::routes())
        .merge(platform::routes())
        .merge(me::routes())
        .merge(tenants::routes())
        .merge(layers::routes())
        .fallback(error::fallback)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state)
}
