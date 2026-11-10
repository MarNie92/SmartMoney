//! Full wallet histories for the smart-money test.
//!
//! The market sample only shows a slice of each wallet's trading. Here the
//! most active wallets of the sample get their complete taker history in the
//! test range, and every market they traded is resolved via Gamma (final
//! prices of resolved markets, see `dataset::gamma_payouts`). Markets from the
//! sample keep their complete trade list; markets known only through the
//! wallets' own trades are marked incomplete, so the copy test skips them.

use crate::dataset::gamma_payouts;
use crate::engine::{HistTrade, MarketCase};
use futures::stream::{self, StreamExt};
use serde::Serialize;
use smartmoney_collector::{ApiError, PolymarketClient};
use smartmoney_core::Side;
use smartmoney_store::MarketRecord;
use std::collections::{BTreeMap, HashMap, HashSet};

const CONCURRENCY: usize = 8;
const GAMMA_BATCH: usize = 40;
const DAY: i64 = 86_400;

#[derive(Debug, Clone)]
pub struct FullHistoryOptions {
    /// Number of most active sample wallets to load.
    pub candidates: usize,
    /// Only trades at or after this time are loaded.
    pub range_start: i64,
    pub now: i64,
    /// Wallets with more pages (1000 trades each) are skipped as bots.
    pub max_pages_per_wallet: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FullHistoryStats {
    pub candidates: usize,
    pub wallets_loaded: usize,
    pub wallets_too_active: usize,
    pub wallets_failed: usize,
    pub wallet_trades: usize,
    pub markets_total: usize,
    pub markets_from_sample: usize,
    pub markets_resolved_extra: usize,
    pub markets_unresolved: usize,
}

/// The `n` wallets with the most trades in the sample (ties by address).
pub fn pick_candidates(cases: &[MarketCase], n: usize) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for case in cases {
        for t in &case.trades {
            *counts.entry(t.wallet.as_str()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    ranked
        .into_iter()
        .take(n)
        .map(|(w, _)| w.to_owned())
        .collect()
}

/// Combine sample markets with wallet-only markets.
///
/// - Sample markets are kept as they are (complete trade lists).
/// - Other markets get the collected wallet trades, sorted by time, and are
///   kept only if they are resolved (`extra` holds record and payouts).
pub fn merge_cases(
    sample: Vec<MarketCase>,
    wallet_trades: HashMap<String, Vec<HistTrade>>,
    extra: HashMap<String, (MarketRecord, Vec<f64>)>,
) -> Vec<MarketCase> {
    let sampled: HashSet<String> = sample
        .iter()
        .map(|c| c.market.condition_id.clone())
        .collect();
    let mut out = sample;
    // BTreeMap for a deterministic order.
    let ordered: BTreeMap<String, Vec<HistTrade>> = wallet_trades.into_iter().collect();
    for (cid, mut trades) in ordered {
        if sampled.contains(&cid) {
            continue;
        }
        let Some((record, payouts)) = extra.get(&cid) else {
            continue;
        };
        trades.sort_by_key(|t| t.timestamp);
        out.push(MarketCase {
            market: record.clone(),
            payouts: payouts.clone(),
            trades,
            control: false,
            trades_complete: false,
        });
    }
    out
}

enum WalletLoad {
    Ok(Vec<(String, HistTrade)>),
    TooActive,
    Failed(ApiError),
}

async fn load_wallet(
    client: &PolymarketClient,
    wallet: &str,
    opts: &FullHistoryOptions,
) -> WalletLoad {
    // Day-granular bounds keep the cache key stable within a day.
    let start = opts.range_start - opts.range_start.rem_euclid(DAY);
    let end = opts.now - opts.now.rem_euclid(DAY) + DAY;
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..opts.max_pages_per_wallet {
        let page = match client
            .user_trades_page(wallet, start, end, cursor.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return WalletLoad::Failed(e),
        };
        for t in &page.data {
            let Ok(side) = t.side.parse::<Side>() else {
                continue;
            };
            out.push((
                t.condition_id.clone(),
                HistTrade {
                    wallet: wallet.to_owned(),
                    side,
                    token_id: t.token_id.clone(),
                    outcome_index: t.outcome_index.filter(|i| *i != 999),
                    price: t.price,
                    size: t.size,
                    timestamp: t.timestamp,
                },
            ));
        }
        match page.next_cursor() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return WalletLoad::Ok(out),
        }
    }
    WalletLoad::TooActive
}

/// True if the wallet has more taker trades in the range than
/// max_pages_per_wallet pages of 1000 (treated as a bot).
pub async fn is_too_active(
    client: &PolymarketClient,
    wallet: &str,
    opts: &FullHistoryOptions,
) -> Result<bool, ApiError> {
    match load_wallet(client, wallet, opts).await {
        WalletLoad::Ok(_) => Ok(false),
        WalletLoad::TooActive => Ok(true),
        WalletLoad::Failed(e) => Err(e),
    }
}

/// Load full histories of the most active sample wallets and merge them
/// with the sample.
pub async fn load(
    client: &PolymarketClient,
    sample: Vec<MarketCase>,
    opts: &FullHistoryOptions,
) -> (Vec<MarketCase>, FullHistoryStats) {
    let mut stats = FullHistoryStats::default();
    let candidates = pick_candidates(&sample, opts.candidates);
    stats.candidates = candidates.len();
    tracing::info!(wallets = candidates.len(), "loading full wallet histories");

    let mut wallet_trades: HashMap<String, Vec<HistTrade>> = HashMap::new();
    let mut results = stream::iter(candidates)
        .map(|w| async move {
            let r = load_wallet(client, &w, opts).await;
            (w, r)
        })
        .buffer_unordered(CONCURRENCY);
    let mut done = 0;
    while let Some((wallet, r)) = results.next().await {
        done += 1;
        match r {
            WalletLoad::Ok(trades) => {
                stats.wallets_loaded += 1;
                stats.wallet_trades += trades.len();
                for (cid, t) in trades {
                    wallet_trades.entry(cid).or_default().push(t);
                }
            }
            WalletLoad::TooActive => stats.wallets_too_active += 1,
            WalletLoad::Failed(e) => {
                tracing::warn!(%wallet, error = %e, "wallet history skipped");
                stats.wallets_failed += 1;
            }
        }
        if done % 100 == 0 {
            tracing::info!(done, "wallet histories loaded");
        }
    }

    let sampled: HashSet<&str> = sample
        .iter()
        .map(|c| c.market.condition_id.as_str())
        .collect();
    let missing: Vec<String> = wallet_trades
        .keys()
        .filter(|cid| !sampled.contains(cid.as_str()))
        .cloned()
        .collect();
    stats.markets_from_sample = wallet_trades
        .keys()
        .filter(|cid| sampled.contains(cid.as_str()))
        .count();
    tracing::info!(markets = missing.len(), "resolving markets via Gamma");

    let mut extra: HashMap<String, (MarketRecord, Vec<f64>)> = HashMap::new();
    let batches: Vec<Vec<String>> = missing.chunks(GAMMA_BATCH).map(|c| c.to_vec()).collect();
    let mut fetched = stream::iter(batches)
        .map(|batch| async move { client.gamma_markets_batch(&batch).await })
        .buffer_unordered(CONCURRENCY);
    let mut batches_done = 0;
    while let Some(r) = fetched.next().await {
        batches_done += 1;
        match r {
            Ok(markets) => {
                for m in markets {
                    let record = m.into_record(None, opts.now);
                    if let Some(payouts) = gamma_payouts(&record) {
                        extra.insert(record.condition_id.clone(), (record, payouts));
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "market batch skipped"),
        }
        if batches_done % 50 == 0 {
            tracing::info!(batches_done, "market batches resolved");
        }
    }
    stats.markets_resolved_extra = extra.len();
    stats.markets_unresolved = missing.len() - extra.len();

    let merged = merge_cases(sample, wallet_trades, extra);
    stats.markets_total = merged.len();
    (merged, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::MarketCategory;

    fn record(cid: &str) -> MarketRecord {
        MarketRecord {
            condition_id: cid.into(),
            gamma_id: None,
            question: None,
            slug: None,
            event_slug: None,
            category: MarketCategory::Other,
            gamma_category: None,
            tags: vec![],
            volume_usd: None,
            volume_24h_usd: None,
            end_date: Some(100),
            closed: true,
            outcomes: vec![],
            outcome_prices: vec![1.0, 0.0],
            clob_token_ids: vec![],
            neg_risk: None,
            resolution_status: Some("resolved".into()),
            raw_json: None,
            fetched_at: 0,
        }
    }

    fn trade(wallet: &str, ts: i64) -> HistTrade {
        HistTrade {
            wallet: wallet.into(),
            side: Side::Buy,
            token_id: "t".into(),
            outcome_index: Some(0),
            price: 0.5,
            size: 10.0,
            timestamp: ts,
        }
    }

    fn case(cid: &str, trades: Vec<HistTrade>) -> MarketCase {
        MarketCase {
            market: record(cid),
            payouts: vec![1.0, 0.0],
            trades,
            control: false,
            trades_complete: true,
        }
    }

    #[test]
    fn picks_most_active_wallets() {
        let cases = vec![
            case("a", vec![trade("x", 1), trade("x", 2), trade("y", 3)]),
            case("b", vec![trade("y", 4), trade("z", 5), trade("x", 6)]),
        ];
        assert_eq!(
            pick_candidates(&cases, 2),
            vec!["x".to_string(), "y".to_string()]
        );
        assert_eq!(pick_candidates(&cases, 10).len(), 3);
    }

    #[test]
    fn merge_keeps_sample_and_adds_resolved_wallet_markets() {
        let sample = vec![case("a", vec![trade("x", 1), trade("other", 2)])];
        let wallet_trades = HashMap::from([
            ("a".to_string(), vec![trade("x", 1)]),
            ("b".to_string(), vec![trade("x", 9), trade("x", 3)]),
            ("c".to_string(), vec![trade("x", 4)]),
        ]);
        // "c" is unresolved -> dropped.
        let extra = HashMap::from([("b".to_string(), (record("b"), vec![1.0, 0.0]))]);
        let merged = merge_cases(sample, wallet_trades, extra);
        assert_eq!(merged.len(), 2);
        let a = merged
            .iter()
            .find(|c| c.market.condition_id == "a")
            .unwrap();
        assert!(a.trades_complete);
        assert_eq!(a.trades.len(), 2, "sample trades kept, not replaced");
        let b = merged
            .iter()
            .find(|c| c.market.condition_id == "b")
            .unwrap();
        assert!(!b.trades_complete);
        assert_eq!(b.trades[0].timestamp, 3, "sorted by time");
    }
}
