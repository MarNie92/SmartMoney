#![forbid(unsafe_code)]

//! SmartMoney daemon. Analysis, signals and paper trading only; it never
//! places real orders.

use anyhow::Context;
use smartmoney_core::Config;
use smartmoney_store::Store;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A local .env is optional; real deployments use systemd EnvironmentFile.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::load().context("loading configuration")?;
    tracing::info!(
        database = %config.database.path.display(),
        min_trade_usd = config.collector.minimum_trade_usd,
        alert_threshold = config.scoring.alert_threshold,
        telegram = config.telegram.bot_token.is_some(),
        dashboard_token = config.dashboard.token.is_some(),
        "configuration loaded"
    );

    let store = Store::open(&config.database.path)
        .await
        .with_context(|| format!("opening database {}", config.database.path.display()))?;
    tracing::info!("database ready, migrations applied");

    // Services (collector, scorer, sim, bot, web) are added in later milestones.
    tracing::info!("SmartMoney daemon running; press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    store.pool().close().await;
    Ok(())
}
