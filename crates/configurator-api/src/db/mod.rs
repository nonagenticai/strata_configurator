//! Database pool and migrations (spec section 10.4). Migrations only, no `create_all`.

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// The schema migrations, embedded at build time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

fn options() -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
}

/// Connect now; fails when the database is unreachable.
pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    options().connect(url).await
}

/// A pool that connects on first use, so a router can be built without a database.
pub fn connect_lazy(url: &str) -> Result<PgPool, sqlx::Error> {
    options().connect_lazy(url)
}

/// Apply every pending migration.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// `true` when a trivial query succeeds.
pub async fn ping(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .is_ok()
}

pub mod axes;
pub mod dimensions;
pub mod layers;
pub mod profiles;
pub mod rows;
pub mod tenants;

/// A snake_case text column as the serde enum it names (`Scope`, `Kind`, `Op`).
pub(crate) fn from_text<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, sqlx::Error> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|e| sqlx::Error::Decode(format!("unexpected value {s:?}: {e}").into()))
}
