use anyhow::Result;
use configurator_api::config::Config;
use configurator_api::routes;
use configurator_api::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cfg = Config::from_env()?;
    let bind = format!("0.0.0.0:{}", cfg.port);
    let app = routes::router(AppState::new(cfg));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "configurator-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
