use std::sync::{Arc, PoisonError, RwLock};

use sqlx::PgPool;

use crate::auth::Verifier;
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
    /// Bearer-token verifier: Keycloak's JWKS in production; tests swap in a static key.
    pub auth: Arc<Verifier>,
}

impl AppState {
    pub fn new(cfg: Config, db: PgPool) -> Self {
        // Same client shape as care-bus: a short timeout, so a hung Keycloak cannot hang requests.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap_or_default();
        let auth = Arc::new(Verifier::jwks(
            &cfg.keycloak_issuer,
            &cfg.keycloak_audience,
            &cfg.keycloak_jwks_url,
            http,
        ));
        Self {
            cfg: Arc::new(cfg),
            db,
            seed: Arc::new(RwLock::new(SeedStatus::Ready)),
            auth,
        }
    }

    /// The same state with tokens verified by `v` instead of Keycloak's JWKS. `main` never calls this.
    pub fn with_verifier(mut self, v: Verifier) -> Self {
        self.auth = Arc::new(v);
        self
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
