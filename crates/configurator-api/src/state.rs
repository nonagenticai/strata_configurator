use std::sync::{Arc, PoisonError, RwLock};

use sqlx::PgPool;

use crate::config::Config;
use crate::db;

/// Whether the seed versions applied cleanly at startup (spec section 4.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedStatus {
    Ready,
    /// Seed `version` would add `key` (e.g. `entity:Order`), which already exists.
    Conflict {
        version: u32,
        key: String,
    },
}

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub seed: Arc<RwLock<SeedStatus>>,
}

impl AppState {
    pub fn new(cfg: Config, db: PgPool) -> Self {
        Self {
            cfg: Arc::new(cfg),
            db,
            seed: Arc::new(RwLock::new(SeedStatus::Ready)),
        }
    }

    /// A state whose pool connects on first use (routes that never touch the DB work without one).
    pub fn lazy(cfg: Config) -> anyhow::Result<Self> {
        let pool = db::connect_lazy(&cfg.database_url)?;
        Ok(Self::new(cfg, pool))
    }

    pub fn seed_status(&self) -> SeedStatus {
        self.seed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_seed_status(&self, status: SeedStatus) {
        *self.seed.write().unwrap_or_else(PoisonError::into_inner) = status;
    }
}
