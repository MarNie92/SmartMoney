#![forbid(unsafe_code)]

//! SmartMoney daemon. Analysis, signals and paper trading only; it never
//! places real orders.

use anyhow::Context;
use smartmoney_collector::{Collector, CollectorEvent, PolymarketClient};
use smartmoney_core::Config;
use smartmoney_scorer::{effective_threshold, Scorer, ScorerEvent};
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
    let (collector_tx, collector_rx) = mpsc::channel::<CollectorEvent>(1024);
    let (scorer_tx, mut scorer_rx) = mpsc::channel::<ScorerEvent>(1024);

    let client = PolymarketClient::new(&config.collector).context("building HTTP client")?;
    let collector = Collector::new(
        client,
        store.clone(),
        config.collector.clone(),
        Some(collector_tx),
    );
    let collector_task = tokio::spawn({
        let shutdown = shutdown_rx.clone();
        async move { collector.run(shutdown).await }
    });

    let scorer = Scorer::new(store.clone(), config.scoring.clone());
    let scorer_task = tokio::spawn(scorer.run(collector_rx, Some(scorer_tx)));

    // Placeholder consumer until the bot and simulation (milestones 5/6) take over.
    let events_task = tokio::spawn({
        let store = store.clone();
        let scoring = config.scoring.clone();
        async move {
            while let Some(event) = scorer_rx.recv().await {
                match event {
                    ScorerEvent::Scored {
                        score_id, total, ..
                    } => {
                        let threshold = effective_threshold(&store, &scoring).await;
                        if let Ok(Some(s)) = store.scored_trade(score_id).await {
                            let line = format!(
                                "score={:.0} [fresh {:.0} asym {:.0} size {:.0} conc {:.0} type {:.0} cluster {:.0}] {} {:.0}$ @ {:.3} {} wallet={}",
                                total,
                                s.wallet_freshness,
                                s.price_asymmetry,
                                s.relative_size,
                                s.concentration,
                                s.market_type,
                                s.cluster_bonus,
                                s.side,
                                s.usdc_value,
                                s.price,
                                s.title.as_deref().unwrap_or("?"),
                                s.proxy_wallet,
                            );
                            if total >= threshold {
                                tracing::warn!("ALERT {line}");
                            } else {
                                tracing::info!("{line}");
                            }
                        }
                    }
                    ScorerEvent::MarketResolved { condition_id } => {
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
    // Collector exit drops its sender, which ends the scorer and the logger.
    let _ = collector_task.await;
    let _ = scorer_task.await;
    let _ = events_task.await;
    store.pool().close().await;
    Ok(())
}
