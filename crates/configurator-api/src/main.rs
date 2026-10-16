use anyhow::Result;
use configurator_api::config::Config;
use configurator_api::seeder::{self, SeedError};
use configurator_api::state::{AppState, SeedStatus};
use configurator_api::{db, routes};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cfg = Config::from_env()?;
    let bind = format!("0.0.0.0:{}", cfg.port);
    let pool = db::connect(&cfg.database_url).await?;
    db::migrate(&pool).await?;
    let state = AppState::new(cfg, pool);

    let bundles = seeder::embedded()?;
    match seeder::apply(&state.db, &bundles).await {
        Ok(applied) => tracing::info!(?applied, "seeds up to date"),
        // Serve anyway: `/ready` reports the conflict and the key it is about.
        Err(SeedError::Conflict { version, key }) => {
            tracing::error!(version, %key, "seed conflict; not ready");
            state.set_seed_status(SeedStatus::Conflict { version, key });
        }
        Err(e) => return Err(e.into()),
    }

    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "configurator-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
