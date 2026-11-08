//! Live scorer: scores each stored trade and forwards results downstream.

use crate::model::{is_fresh, score, ScoreBreakdown, ScoreInput, TradeFacts, SCORER_VERSION};
use smartmoney_collector::CollectorEvent;
use smartmoney_core::config::ScoringConfig;
use smartmoney_core::Side;
use smartmoney_store::{NewScore, Store, StoreError};
use tokio::sync::mpsc;

/// Runtime override of the alert threshold (set via Telegram `/threshold`).
pub const STATE_ALERT_THRESHOLD: &str = "scoring.alert_threshold";
const BACKFILL_BATCH: i64 = 500;

/// Emitted downstream (bot, simulation).
#[derive(Debug, Clone, PartialEq)]
pub enum ScorerEvent {
    Scored {
        score_id: i64,
        trade_id: i64,
        total: f64,
        /// Order book snapshot taken when the trade was detected.
        snapshot_id: Option<i64>,
    },
    MarketResolved {
        condition_id: String,
    },
}

#[derive(Clone)]
pub struct Scorer {
    store: Store,
    cfg: ScoringConfig,
}

impl Scorer {
    pub fn new(store: Store, cfg: ScoringConfig) -> Self {
        Self { store, cfg }
    }

    /// Alert threshold: runtime override from the store, else the config.
    pub async fn alert_threshold(&self) -> f64 {
        effective_threshold(&self.store, &self.cfg).await
    }

    /// Score one stored trade and persist the result.
    pub async fn score_trade(
        &self,
        trade_id: i64,
    ) -> Result<Option<(i64, ScoreBreakdown)>, StoreError> {
        let Some(trade) = self.store.trade(trade_id).await? else {
            return Ok(None);
        };
        let side: Side = match trade.side.parse() {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let facts = TradeFacts {
            side,
            condition_id: trade.condition_id.clone(),
            price: trade.price,
            usdc_value: trade.usdc_value,
            timestamp: trade.timestamp,
        };
        let wallet = self.store.wallet(&trade.proxy_wallet).await?;
        let market = self
            .store
            .market(&trade.condition_id)
            .await?
            .map(|m| m.market);
        let positions = self.store.wallet_positions(&trade.proxy_wallet).await?;
        let fresh_cluster_wallets = if side == Side::Buy {
            self.fresh_buyers(&trade.token_id, trade.timestamp).await?
        } else {
            0
        };

        let breakdown = score(
            &ScoreInput {
                trade: &facts,
                wallet: wallet.as_ref(),
                market: market.as_ref(),
                positions: &positions,
                fresh_cluster_wallets,
            },
            &self.cfg,
        );
        let score_id = self
            .store
            .upsert_score(&NewScore {
                trade_id,
                proxy_wallet: trade.proxy_wallet,
                condition_id: trade.condition_id,
                total: breakdown.total,
                wallet_freshness: breakdown.wallet_freshness,
                price_asymmetry: breakdown.price_asymmetry,
                relative_size: breakdown.relative_size,
                concentration: breakdown.concentration,
                market_type: breakdown.market_type,
                cluster_bonus: breakdown.cluster_bonus,
                details_json: breakdown.details.to_string(),
                scorer_version: SCORER_VERSION,
                created_at: chrono::Utc::now().timestamp(),
            })
            .await?;
        Ok(Some((score_id, breakdown)))
    }

    /// Fresh wallets that bought the same outcome token in the window ending
    /// at `ts` (no lookahead), including the trading wallet itself.
    async fn fresh_buyers(&self, token_id: &str, ts: i64) -> Result<usize, StoreError> {
        let window = (self.cfg.cluster_window_minutes * 60) as i64;
        let buyers = self
            .store
            .buyers_in_window(token_id, ts - window, ts)
            .await?;
        let mut fresh = 0;
        for (address, first_buy) in buyers {
            let wallet = self.store.wallet(&address).await?;
            if is_fresh(wallet.as_ref(), first_buy, &self.cfg) {
                fresh += 1;
            }
        }
        Ok(fresh)
    }

    /// Score trades left unscored (e.g. stored before a restart).
    pub async fn backfill(&self) -> Result<usize, StoreError> {
        let mut done = 0;
        loop {
            let ids = self.store.unscored_trade_ids(BACKFILL_BATCH).await?;
            if ids.is_empty() {
                return Ok(done);
            }
            let batch = ids.len();
            for id in ids {
                self.score_trade(id).await?;
            }
            done += batch;
        }
    }

    /// Consume collector events until the channel closes.
    pub async fn run(
        self,
        mut events: mpsc::Receiver<CollectorEvent>,
        downstream: Option<mpsc::Sender<ScorerEvent>>,
    ) {
        match self.backfill().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(scored = n, "backfilled unscored trades"),
            Err(e) => tracing::error!(error = %e, "score backfill failed"),
        }
        tracing::info!("scorer started");
        while let Some(event) = events.recv().await {
            let out = match event {
                CollectorEvent::TradeStored {
                    trade_id,
                    snapshot_id,
                } => match self.score_trade(trade_id).await {
                    Ok(Some((score_id, b))) => {
                        let threshold = self.alert_threshold().await;
                        if b.total >= threshold {
                            tracing::info!(trade_id, score = b.total, "high score");
                        } else {
                            tracing::debug!(trade_id, score = b.total, "scored");
                        }
                        Some(ScorerEvent::Scored {
                            score_id,
                            trade_id,
                            total: b.total,
                            snapshot_id,
                        })
                    }
                    Ok(None) => None,
                    Err(e) => {
                        tracing::error!(trade_id, error = %e, "scoring failed");
                        None
                    }
                },
                CollectorEvent::MarketResolved { condition_id } => {
                    Some(ScorerEvent::MarketResolved { condition_id })
                }
            };
            if let (Some(tx), Some(event)) = (&downstream, out) {
                if tx.send(event).await.is_err() {
                    tracing::debug!("scorer downstream receiver dropped");
                }
            }
        }
        tracing::info!("scorer stopped");
    }
}

/// Alert threshold: runtime override from `kv_state`, else the config value.
pub async fn effective_threshold(store: &Store, cfg: &ScoringConfig) -> f64 {
    match store.get_state(STATE_ALERT_THRESHOLD).await {
        Ok(Some(v)) => v.parse().unwrap_or(cfg.alert_threshold),
        _ => cfg.alert_threshold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::MarketCategory;
    use smartmoney_store::{MarketRecord, NewTrade, WalletRecord};

    const T0: i64 = 1_800_000_000;

    async fn add_trade(
        store: &Store,
        hash: &str,
        wallet: &str,
        side: Side,
        ts: i64,
        price: f64,
    ) -> i64 {
        store
            .insert_trade(
                &NewTrade {
                    transaction_hash: hash.into(),
                    proxy_wallet: wallet.into(),
                    condition_id: "0xm".into(),
                    token_id: "tok-yes".into(),
                    side,
                    outcome: Some("Yes".into()),
                    outcome_index: Some(0),
                    size: 5_000.0 / price,
                    price,
                    timestamp: ts,
                    title: Some("Will the CEO resign?".into()),
                    slug: None,
                    event_slug: None,
                    raw_json: None,
                },
                ts,
            )
            .await
            .unwrap()
            .unwrap()
    }

    async fn add_wallet(store: &Store, address: &str, age_days: i64, markets: i64) {
        store
            .upsert_wallet(&WalletRecord {
                address: address.into(),
                join_date: Some(T0 - age_days * 86_400),
                markets_traded: Some(markets),
                enriched_at: T0,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    async fn setup() -> (Store, Scorer) {
        let store = Store::open_in_memory().await.unwrap();
        store
            .upsert_market(&MarketRecord {
                condition_id: "0xm".into(),
                gamma_id: None,
                question: Some("Will the CEO resign?".into()),
                slug: None,
                event_slug: None,
                category: MarketCategory::Personnel,
                gamma_category: None,
                tags: vec![],
                volume_usd: None,
                volume_24h_usd: Some(20_000.0),
                end_date: None,
                closed: false,
                outcomes: vec![],
                outcome_prices: vec![],
                clob_token_ids: vec![],
                neg_risk: None,
                resolution_status: None,
                raw_json: None,
                fetched_at: T0,
            })
            .await
            .unwrap();
        let scorer = Scorer::new(store.clone(), ScoringConfig::default());
        (store, scorer)
    }

    #[tokio::test]
    async fn scores_and_persists_with_cluster() {
        let (store, scorer) = setup().await;
        add_wallet(&store, "0xa", 1, 1).await;
        add_wallet(&store, "0xb", 2, 2).await;
        add_wallet(&store, "0xold", 500, 200).await;

        let first = add_trade(&store, "0x1", "0xa", Side::Buy, T0, 0.04).await;
        let (_, b1) = scorer.score_trade(first).await.unwrap().unwrap();
        assert_eq!(b1.cluster_bonus, 0.0, "only one fresh wallet so far");

        // An old wallet buying does not count toward the cluster.
        add_trade(&store, "0x2", "0xold", Side::Buy, T0 + 60, 0.04).await;
        let second = add_trade(&store, "0x3", "0xb", Side::Buy, T0 + 600, 0.04).await;
        let (score_id, b2) = scorer.score_trade(second).await.unwrap().unwrap();
        assert_eq!(b2.cluster_bonus, 10.0);
        // Fresh 25 + asym 25 + size 20 (5k of 20k) + conc 15 + personnel 15 + cluster 10.
        assert_eq!(b2.total, 100.0);

        let stored = store.scored_trade(score_id).await.unwrap().unwrap();
        assert_eq!(stored.total, 100.0);
        assert_eq!(stored.category.as_deref(), Some("personnel"));
        let details: serde_json::Value = serde_json::from_str(&stored.details_json).unwrap();
        assert_eq!(details["cluster_bonus"]["fresh_wallets"], 2);
    }

    #[tokio::test]
    async fn cluster_window_excludes_old_buys() {
        let (store, scorer) = setup().await;
        add_wallet(&store, "0xa", 1, 1).await;
        add_wallet(&store, "0xb", 1, 1).await;
        add_trade(&store, "0x1", "0xa", Side::Buy, T0, 0.04).await;
        // 31 minutes later: outside the 30-minute window.
        let late = add_trade(&store, "0x2", "0xb", Side::Buy, T0 + 31 * 60, 0.04).await;
        let (_, b) = scorer.score_trade(late).await.unwrap().unwrap();
        assert_eq!(b.cluster_bonus, 0.0);
    }

    #[tokio::test]
    async fn threshold_override_and_backfill() {
        let (store, scorer) = setup().await;
        assert_eq!(scorer.alert_threshold().await, 60.0);
        store
            .set_state(STATE_ALERT_THRESHOLD, "75", T0)
            .await
            .unwrap();
        assert_eq!(scorer.alert_threshold().await, 75.0);

        add_trade(&store, "0x1", "0xa", Side::Buy, T0, 0.5).await;
        add_trade(&store, "0x2", "0xa", Side::Sell, T0 + 1, 0.5).await;
        assert_eq!(scorer.backfill().await.unwrap(), 2);
        assert_eq!(scorer.backfill().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn run_forwards_events() {
        let (store, scorer) = setup().await;
        let id = add_trade(&store, "0x1", "0xa", Side::Buy, T0, 0.5).await;
        let (ctx, crx) = mpsc::channel(4);
        let (stx, mut srx) = mpsc::channel(4);
        // Score via the event, not the backfill: insert after starting.
        let handle = tokio::spawn(scorer.run(crx, Some(stx)));
        ctx.send(CollectorEvent::TradeStored {
            trade_id: id,
            snapshot_id: Some(7),
        })
        .await
        .unwrap();
        ctx.send(CollectorEvent::MarketResolved {
            condition_id: "0xm".into(),
        })
        .await
        .unwrap();
        drop(ctx);
        handle.await.unwrap();

        let mut got = Vec::new();
        while let Ok(e) = srx.try_recv() {
            got.push(e);
        }
        assert!(got
            .iter()
            .any(|e| matches!(e, ScorerEvent::Scored { trade_id, snapshot_id: Some(7), .. } if *trade_id == id)));
        assert!(got
            .iter()
            .any(|e| matches!(e, ScorerEvent::MarketResolved { .. })));
    }
}
