use axum::Router;

use crate::error;
use crate::state::AppState;

pub mod catalog;
pub mod compile;
pub mod health;
pub mod layers;
pub mod license;
pub mod me;
pub mod platform;
pub mod releases;
pub mod signups;
pub mod tenants;

pub fn router(state: AppState) -> Router {
    let app = Router::new()
        .merge(health::routes())
        .merge(catalog::routes())
        .merge(compile::routes())
        .merge(platform::routes())
        .merge(me::routes())
        .merge(license::routes())
        .merge(tenants::routes())
        .merge(layers::routes())
        .merge(releases::routes());
    // Early access ships dark: unless it is switched on, its paths are the unknown-route 404.
    let app = if state.cfg.signup.enabled {
        app.merge(signups::routes())
    } else {
        app
    };
    app.fallback(error::fallback)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state)
}
