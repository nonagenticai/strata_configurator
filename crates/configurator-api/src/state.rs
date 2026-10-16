use std::sync::Arc;

use crate::config::Config;

/// Shared handler state. The database pool arrives with the persistence task.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        Self { cfg: Arc::new(cfg) }
    }
}
