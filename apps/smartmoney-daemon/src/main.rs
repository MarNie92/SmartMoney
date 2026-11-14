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

    // Validate the dashboard bind policy before starting anything else.
    smartmoney_web::resolve_bind(&config.dashboard).context("dashboard configuration")?;
    let web_state = smartmoney_web::AppState {
        store: store.clone(),
        scoring: config.scoring.clone(),
        token: config
            .dashboard
            .token
            .as_ref()
            .map(|t| t.expose().to_owned()),
    };
    let web_task = tokio::spawn({
        let shutdown = shutdown_rx.clone();
        let cfg = config.dashboard.clone();
        async move {
            if let Err(e) = smartmoney_web::serve(&cfg, web_state, shutdown).await {
                tracing::error!(error = %e, "dashboard stopped");
            }
        }
    });

    let scorer = Scorer::new(store.clone(), config.scoring.clone());
    let scorer_task = tokio::spawn(scorer.run(collector_rx, Some(scorer_tx)));

    // Paper trading (simulation only; never places real orders).
    let (sim_tx, mut sim_rx) = mpsc::channel::<smartmoney_sim::SimEvent>(256);
    let sim_client =
        PolymarketClient::new(&config.collector).context("building sim HTTP client")?;
    let sim = std::sync::Arc::new(
        smartmoney_sim::SimService::new(
            store.clone(),
            sim_client,
            config.simulation.clone(),
            chrono::Utc::now().timestamp(),
            Some(sim_tx),
        )
        .await
        .context("starting paper trading")?,
    );
    tracing::info!(
        favorites98 = sim.favorites_active(),
        anchor2 = sim.anchor_active(),
        "paper trading ready"
    );
    let sim_task = tokio::spawn(sim.clone().run(shutdown_rx.clone()));

    // Telegram alerts (optional: needs SMARTMONEY_TELEGRAM_BOT_TOKEN).
    let bot = match &config.telegram.bot_token {
        Some(token) => {
            let client = smartmoney_bot::TelegramClient::new(token.clone())
                .context("building telegram client")?;
            let bot = std::sync::Arc::new(smartmoney_bot::Bot::new(
                client,
                store.clone(),
                config.telegram.allowed_chat_ids.clone(),
            ));
            tracing::info!(
                chats = config.telegram.allowed_chat_ids.len(),
                "telegram bot active"
            );
            Some(bot)
        }
        None => None,
    };
    let bot_task = bot
        .clone()
        .map(|b| tokio::spawn(b.run(shutdown_rx.clone())));

    // Logger and paper-trading consumer; the Telegram bot (milestone 5)
    // will join as a further consumer.
    let events_task = tokio::spawn({
        let store = store.clone();
        let scoring = config.scoring.clone();
        let sim = sim.clone();
        let bot = bot.clone();
        async move {
            loop {
                let event = tokio::select! {
                    ev = scorer_rx.recv() => match ev {
                        Some(e) => e,
                        None => break,
                    },
                    ev = sim_rx.recv() => {
                        if let Some(e) = ev {
                            if let Some(bot) = &bot {
                                match &e {
                                    smartmoney_sim::SimEvent::Opened {
                                        strategy, label, shares, avg_price, cost_usd, fee_usd,
                                    } => {
                                        bot.notify_fill(
                                            strategy, label, *shares, *avg_price, *cost_usd,
                                            *fee_usd,
                                        )
                                        .await;
                                    }
                                    smartmoney_sim::SimEvent::Settled {
                                        strategy, title, shares, payout_usd, realized_pnl,
                                    } => {
                                        bot.notify_settle(
                                            strategy, title, *shares, *payout_usd, *realized_pnl,
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        continue;
                    }
                };
                sim.handle_event(&event).await;
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
                                if let Some(bot) = &bot {
                                    bot.notify_alert(&s).await;
                                }
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
    let _ = web_task.await;
    let _ = collector_task.await;
    let _ = scorer_task.await;
    let _ = events_task.await;
    let _ = sim_task.await;
    if let Some(t) = bot_task {
        let _ = t.await;
    }
    store.pool().close().await;
    Ok(())
}
