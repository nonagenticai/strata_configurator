//! Database pool and migrations (spec section 10.4). Migrations only, no `create_all`.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

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

/// Refuse to serve over a connection that RLS cannot hold: the runtime role must not be a superuser,
/// must not have BYPASSRLS, must not own the database and must not own any table under RLS (an owner
/// bypasses ENABLEd RLS). There is deliberately no flag to skip this.
pub async fn assert_runtime_role(pool: &PgPool) -> anyhow::Result<()> {
    let (user, superuser, bypass, owns_db, owns_rls_table) =
        sqlx::query_as::<_, (String, bool, bool, bool, bool)>(
            "SELECT r.rolname::text, r.rolsuper, r.rolbypassrls, d.datdba = r.oid, \
                    EXISTS (SELECT 1 FROM pg_class c \
                            JOIN pg_namespace n ON n.oid = c.relnamespace \
                            WHERE n.nspname = 'public' AND c.relowner = r.oid AND c.relrowsecurity) \
             FROM pg_roles r, pg_database d \
             WHERE r.rolname = current_user AND d.datname = current_database()",
        )
        .fetch_one(pool)
        .await?;
    let mut why = Vec::new();
    if superuser {
        why.push("is a superuser");
    }
    if bypass {
        why.push("has BYPASSRLS");
    }
    if owns_db {
        why.push("owns the database");
    }
    if owns_rls_table {
        why.push("owns a table under row-level security");
    }
    if why.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "DATABASE_URL connects as {user:?}, which {}: tenant isolation (RLS) would not apply. \
             Connect as the non-bypass app role (configurator_app) and put the owner in \
             MIGRATE_DATABASE_URL",
            why.join(", ")
        )
    }
}

/// Set the tenant this transaction acts in (`app.current_tenant`, transaction-local, so it never
/// leaks to the next user of a pooled or pgbouncer connection). Must run inside a transaction.
pub async fn set_tenant(conn: &mut PgConnection, tenant: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

/// A transaction on `pool` acting in `tenant`: every runtime-pool access made for a tenant goes
/// through this, so RLS sees the tenant.
pub async fn tenant_tx(
    pool: &PgPool,
    tenant: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    set_tenant(&mut tx, tenant).await?;
    Ok(tx)
}

pub mod axes;
pub mod dimensions;
pub mod layers;
pub mod licenses;
pub mod profiles;
pub mod rows;
pub mod tenants;
pub mod users;

/// A snake_case text column as the serde enum it names (`Scope`, `Kind`, `Op`).
pub(crate) fn from_text<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, sqlx::Error> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|e| sqlx::Error::Decode(format!("unexpected value {s:?}: {e}").into()))
}
