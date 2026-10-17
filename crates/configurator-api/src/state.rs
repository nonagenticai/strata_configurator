use std::sync::{Arc, PoisonError, RwLock};

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::Verifier;
use crate::config::Config;
use crate::db;
use crate::keycloak_admin::KeycloakAdmin;
use crate::license::LicenseGate;
use crate::licenses_admin::LicensesAdmin;
use crate::signup::Signup;

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
    /// The runtime pool: the non-bypass app role, under RLS. Tenant work goes through
    /// [`db::tenant_tx`] (or [`AppState::tx`]) so `app.current_tenant` is set.
    pub db: PgPool,
    /// The owner pool (bypasses RLS): migrations, the seeder and super-admin platform work only.
    pub owner: PgPool,
    pub seed: Arc<RwLock<SeedStatus>>,
    /// Bearer-token verifier: Keycloak's JWKS in production; tests swap in a static key.
    pub auth: Arc<Verifier>,
    /// The portal-realm admin SA that invites people by email; `None` when not configured.
    pub keycloak_admin: Option<Arc<KeycloakAdmin>>,
    /// Licence gate 2 (a no-op unless `LICENSE_GATE_2_ENABLED`).
    pub license: Arc<LicenseGate>,
    /// The `secure-apps` SA that issues tenant licences; `None` when not configured.
    pub licenses_admin: Option<Arc<LicensesAdmin>>,
    /// Early-access signup settings, Turnstile and Telegram (W2.3).
    pub signup: Arc<Signup>,
}

impl AppState {
    pub fn new(cfg: Config, db: PgPool, owner: PgPool) -> Self {
        // Same client shape as care-bus: a short timeout, so a hung Keycloak cannot hang requests.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap_or_default();
        let keycloak_admin = cfg
            .keycloak_admin
            .as_ref()
            .map(|k| Arc::new(KeycloakAdmin::new(k, http.clone())));
        let license = Arc::new(LicenseGate::new(cfg.license_gate.clone(), http.clone()));
        // Licences record the realm the holder signs in with: the last segment of the issuer.
        let realm = cfg
            .keycloak_issuer
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let licenses_admin = cfg.licenses_admin.as_ref().map(|l| {
            Arc::new(LicensesAdmin::new(
                l,
                &cfg.license_gate.app_key,
                realm,
                http.clone(),
            ))
        });
        let signup = Arc::new(Signup::new(cfg.signup.clone(), http.clone()));
        let auth = Arc::new(Verifier::jwks(
            &cfg.keycloak_issuer,
            &cfg.keycloak_audience,
            &cfg.keycloak_jwks_url,
            http,
        ));
        Self {
            cfg: Arc::new(cfg),
            db,
            owner,
            seed: Arc::new(RwLock::new(SeedStatus::Ready)),
            auth,
            keycloak_admin,
            license,
            licenses_admin,
            signup,
        }
    }

    /// The same state with tokens verified by `v` instead of Keycloak's JWKS. `main` never calls this.
    pub fn with_verifier(mut self, v: Verifier) -> Self {
        self.auth = Arc::new(v);
        self
    }

    /// A state whose pools connect on first use (routes that never touch the DB work without one).
    pub fn lazy(cfg: Config) -> anyhow::Result<Self> {
        let pool = db::connect_lazy(&cfg.database_url)?;
        let owner = db::connect_lazy(cfg.owner_database_url())?;
        Ok(Self::new(cfg, pool, owner))
    }

    /// A transaction over the data of `tenant`: for a tenant, the runtime pool with
    /// `app.current_tenant` set (RLS applies); for `None` (platform data), the owner pool. Callers
    /// authorise first: platform writes need the super-admin, and only the super-admin acts with no
    /// tenant.
    pub async fn tx(
        &self,
        tenant: Option<Uuid>,
    ) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
        match tenant {
            Some(t) => db::tenant_tx(&self.db, t).await,
            None => self.owner.begin().await,
        }
    }

    /// [`AppState::tx`] as one consistent read-only snapshot (`REPEATABLE READ, READ ONLY`). The
    /// isolation level must be set before any other statement, so the tenant is set after it.
    pub async fn snapshot(
        &self,
        tenant: Option<Uuid>,
    ) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
        let mut tx = match tenant {
            Some(_) => self.db.begin().await?,
            None => self.owner.begin().await?,
        };
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        if let Some(t) = tenant {
            db::set_tenant(&mut tx, t).await?;
        }
        Ok(tx)
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
