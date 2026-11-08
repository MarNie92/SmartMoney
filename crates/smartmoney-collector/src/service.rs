//! Polling loop: fetch large trades, store them, enrich wallets and markets,
//! snapshot order books and track market resolutions.

use crate::client::{ApiError, PolymarketClient};
use smartmoney_core::config::CollectorConfig;
use smartmoney_store::{Store, StoreError, WalletRecord};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// State key for the newest trade timestamp seen.
pub const STATE_LAST_TRADE_TS: &str = "collector.trades.last_ts";
/// Re-read trades this many seconds older than the newest one seen, so trades
/// that appear late in the feed are not missed (duplicates are ignored).
const OVERLAP_SECONDS: i64 = 300;
const PAGE_LIMIT: u32 = 100;
/// Page cap per poll (first run: only one page, no deep backfill).
const MAX_PAGES: usize = 10;
const POSITIONS_LIMIT: u32 = 100;
const RESOLUTION_INTERVAL: Duration = Duration::from_secs(600);
const RESOLUTION_BATCH: i64 = 25;

#[derive(Debug, thiserror::Error)]
pub enum CollectorError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Emitted for every newly stored (and enriched) trade.
#[derive(Debug, Clone, PartialEq)]
pub enum CollectorEvent {
    TradeStored {
        trade_id: i64,
        /// Order book snapshot of the traded token at detection time.
        snapshot_id: Option<i64>,
    },
    MarketResolved {
        condition_id: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct PollStats {
    pub pages: usize,
    pub seen: usize,
    pub new_trades: Vec<i64>,
}

pub struct Collector {
    client: PolymarketClient,
    store: Store,
    cfg: CollectorConfig,
    events: Option<mpsc::Sender<CollectorEvent>>,
}

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Collector {
    pub fn new(
        client: PolymarketClient,
        store: Store,
        cfg: CollectorConfig,
        events: Option<mpsc::Sender<CollectorEvent>>,
    ) -> Self {
        Self {
            client,
            store,
            cfg,
            events,
        }
    }

    pub fn client(&self) -> &PolymarketClient {
        &self.client
    }

    /// Run until `shutdown` flips to true.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let mut poll = tokio::time::interval(Duration::from_secs(self.cfg.poll_interval_seconds));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut resolve = tokio::time::interval(RESOLUTION_INTERVAL);
        resolve.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(
            min_usd = self.cfg.minimum_trade_usd,
            interval_s = self.cfg.poll_interval_seconds,
            "collector started"
        );
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
                _ = poll.tick() => {
                    match self.poll_once().await {
                        Ok(stats) if !stats.new_trades.is_empty() => tracing::info!(
                            pages = stats.pages, seen = stats.seen, new = stats.new_trades.len(),
                            "collected trades"
                        ),
                        Ok(stats) => tracing::debug!(pages = stats.pages, seen = stats.seen, "no new trades"),
                        Err(e) => tracing::error!(error = %e, "trade poll failed"),
                    }
                }
                _ = resolve.tick() => {
                    match self.check_resolutions().await {
                        Ok(n) if n > 0 => tracing::info!(resolved = n, "markets resolved"),
                        Ok(_) => {}
                        Err(e) => tracing::error!(error = %e, "resolution check failed"),
                    }
                }
            }
        }
        tracing::info!("collector stopped");
    }

    /// One poll: fetch new large trades, store and enrich them.
    pub async fn poll_once(&self) -> Result<PollStats, CollectorError> {
        let last_ts: Option<i64> = self
            .store
            .get_state(STATE_LAST_TRADE_TS)
            .await?
            .and_then(|v| v.parse().ok());
        let stop_before = last_ts.map(|ts| ts - OVERLAP_SECONDS);
        let max_pages = if last_ts.is_some() { MAX_PAGES } else { 1 };

        let mut stats = PollStats::default();
        let mut newest = last_ts;
        let mut cursor: Option<String> = None;
        let mut to_enrich = Vec::new();

        while stats.pages < max_pages {
            let (page, raws) = self
                .client
                .trades_page(self.cfg.minimum_trade_usd, PAGE_LIMIT, cursor.as_deref())
                .await?;
            stats.pages += 1;
            let mut reached_known = false;
            let ingested_at = now_ts();
            for (i, trade) in page.data.iter().enumerate() {
                stats.seen += 1;
                newest = Some(newest.map_or(trade.timestamp, |n| n.max(trade.timestamp)));
                if stop_before.is_some_and(|s| trade.timestamp < s) {
                    reached_known = true;
                    continue;
                }
                // The API filters already; guard against filter drift.
                if trade.usdc_value() < self.cfg.minimum_trade_usd {
                    continue;
                }
                let raw = raws.get(i).map(|v| v.to_string());
                let new_trade = match trade.clone().into_new_trade(raw) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(error = %e, tx = %trade.transaction_hash, "skipping trade");
                        continue;
                    }
                };
                if let Some(id) = self.store.insert_trade(&new_trade, ingested_at).await? {
                    stats.new_trades.push(id);
                    to_enrich.push((id, new_trade));
                }
            }
            match page.next_cursor() {
                Some(c) if !reached_known => cursor = Some(c.to_owned()),
                _ => break,
            }
        }

        if let Some(ts) = newest {
            self.store
                .set_state(STATE_LAST_TRADE_TS, &ts.to_string(), now_ts())
                .await?;
        }

        // Oldest first so events reach the scorer in chronological order.
        to_enrich.sort_by_key(|(id, t)| (t.timestamp, *id));
        for (id, trade) in to_enrich {
            if let Err(e) = self.enrich_market(&trade.condition_id).await {
                tracing::warn!(error = %e, condition = %trade.condition_id, "market enrichment failed");
            }
            if let Err(e) = self.enrich_wallet(&trade.proxy_wallet).await {
                tracing::warn!(error = %e, wallet = %trade.proxy_wallet, "wallet enrichment failed");
            }
            let snapshot_id = match self.snapshot_book(&trade.token_id).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(error = %e, token = %trade.token_id, "order book snapshot failed");
                    None
                }
            };
            self.emit(CollectorEvent::TradeStored {
                trade_id: id,
                snapshot_id,
            })
            .await;
        }
        Ok(stats)
    }

    fn is_fresh(&self, fetched_at: Option<i64>) -> bool {
        let max_age = (self.cfg.enrich_cache_minutes * 60) as i64;
        fetched_at.is_some_and(|ts| now_ts() - ts < max_age)
    }

    /// Fetch market data from Gamma unless cached recently.
    pub async fn enrich_market(&self, condition_id: &str) -> Result<bool, CollectorError> {
        if self.is_fresh(self.store.market_fetched_at(condition_id).await?) {
            return Ok(false);
        }
        let Some(market) = self.client.gamma_market(condition_id).await? else {
            tracing::warn!(condition = %condition_id, "market not found on Gamma");
            return Ok(false);
        };
        let record = market
            .value
            .into_record(Some(market.raw.to_string()), now_ts());
        self.store.upsert_market(&record).await?;
        Ok(true)
    }

    /// Fetch wallet stats, age and positions unless cached recently.
    pub async fn enrich_wallet(&self, address: &str) -> Result<bool, CollectorError> {
        if self.is_fresh(self.store.wallet_enriched_at(address).await?) {
            return Ok(false);
        }
        let stats = self.client.user_stats(address).await?;
        let mut record = WalletRecord {
            address: address.to_owned(),
            raw_stats_json: Some(stats.raw.to_string()),
            enriched_at: now_ts(),
            ..Default::default()
        };
        if let Some(s) = &stats.value {
            record.join_date = s.join_date;
            record.markets_traded = s.trades;
            record.trade_count = s.trade_count();
        }
        let known_first = self
            .store
            .wallet(address)
            .await?
            .and_then(|w| w.first_activity_at);
        if record.join_date.is_none() && known_first.is_none() {
            if let Some(first) = self.client.first_activity(address).await? {
                record.first_activity_at = Some(first.timestamp);
                record.name = first.name.filter(|n| !n.is_empty());
                record.pseudonym = first.pseudonym.filter(|n| !n.is_empty());
            }
        }

        let positions: Vec<_> = self
            .client
            .positions(address, POSITIONS_LIMIT)
            .await?
            .into_iter()
            .map(smartmoney_store::PositionRecord::from)
            .collect();
        let values = positions.iter().filter_map(|p| p.current_value);
        record.total_positions_usd = Some(values.clone().sum());
        record.largest_position_usd =
            values.fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));

        self.store.upsert_wallet(&record).await?;
        self.store
            .replace_wallet_positions(address, &positions, record.enriched_at)
            .await?;
        Ok(true)
    }

    pub async fn snapshot_book(&self, token_id: &str) -> Result<i64, CollectorError> {
        let book = self.client.book(token_id).await?;
        let id = self
            .store
            .insert_book_snapshot(&book.to_snapshot(now_ts()))
            .await?;
        Ok(id)
    }

    /// Refresh markets past their end date and record final payouts.
    pub async fn check_resolutions(&self) -> Result<usize, CollectorError> {
        let due = self
            .store
            .markets_awaiting_resolution(now_ts(), RESOLUTION_BATCH)
            .await?;
        let mut resolved = 0;
        for condition_id in due {
            // Refresh Gamma data (closed flag, final prices); bumps fetched_at
            // so the batch rotates through all pending markets.
            if let Some(m) = self.client.gamma_market(&condition_id).await? {
                let record = m.value.into_record(Some(m.raw.to_string()), now_ts());
                self.store.upsert_market(&record).await?;
            }
            let Some(row) = self.client.resolution(&condition_id).await? else {
                continue;
            };
            if let Some(resolution) = row.to_resolution() {
                self.store
                    .set_market_resolution(&condition_id, &resolution)
                    .await?;
                resolved += 1;
                self.emit(CollectorEvent::MarketResolved { condition_id })
                    .await;
            }
        }
        Ok(resolved)
    }

    async fn emit(&self, event: CollectorEvent) {
        if let Some(tx) = &self.events {
            if tx.send(event).await.is_err() {
                tracing::debug!("collector event receiver dropped");
            }
        }
    }
}
