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
    // The owner migrates and seeds; requests run as the non-bypass app role, under RLS.
    let owner = db::connect(cfg.owner_database_url()).await?;
    db::migrate(&owner).await?;
    let pool = db::connect(&cfg.database_url).await?;
    db::assert_runtime_role(&pool).await?;
    let state = AppState::new(cfg, pool, owner);

    let bundles = seeder::embedded()?;
    match seeder::apply(&state.owner, &bundles).await {
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
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = interrupt => {}, _ = terminate => {} }
}
