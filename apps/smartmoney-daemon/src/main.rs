#![forbid(unsafe_code)]

//! SmartMoney daemon. Analysis, signals and paper trading only; it never
//! places real orders.

use anyhow::Context;
use smartmoney_collector::{Collector, CollectorEvent, PolymarketClient};
use smartmoney_core::Config;
use smartmoney_store::Store;
use tokio::sync::{mpsc, watch};
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

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (event_tx, mut event_rx) = mpsc::channel::<CollectorEvent>(1024);

    let client = PolymarketClient::new(&config.collector).context("building HTTP client")?;
    let collector = Collector::new(
        client,
        store.clone(),
        config.collector.clone(),
        Some(event_tx),
    );
    let collector_task = tokio::spawn({
        let shutdown = shutdown_rx.clone();
        async move { collector.run(shutdown).await }
    });

    // Placeholder consumer until the scorer (milestone 3) takes over.
    let events_task = tokio::spawn({
        let store = store.clone();
        async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    CollectorEvent::TradeStored { trade_id, .. } => {
                        if let Ok(Some(t)) = store.trade(trade_id).await {
                            tracing::info!(
                                wallet = %t.proxy_wallet,
                                side = %t.side,
                                usd = format!("{:.0}", t.usdc_value),
                                price = t.price,
                                market = t.title.as_deref().unwrap_or("?"),
                                "large trade"
                            );
                        }
                    }
                    CollectorEvent::MarketResolved { condition_id } => {
                        tracing::info!(%condition_id, "market resolved");
                    }
                }
            }
        }
    });

    tracing::info!("SmartMoney daemon running; press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    let _ = shutdown_tx.send(true);
    let _ = collector_task.await;
    events_task.abort();
    store.pool().close().await;
    Ok(())
}
