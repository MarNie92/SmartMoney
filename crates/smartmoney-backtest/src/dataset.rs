//! Fetch resolved markets, their trades and wallet facts (cached on disk).

use crate::engine::{candidate_wallets, HistTrade, MarketCase, WalletInfo};
use futures::stream::{self, StreamExt};
use smartmoney_collector::{ApiError, PolymarketClient};
use smartmoney_core::config::BacktestConfig;
use smartmoney_core::Side;
use std::collections::{HashMap, HashSet};

const DAY: i64 = 86_400;
const EVENTS_PAGE: usize = 50;
const MAX_EVENT_PAGES: usize = 20;
const CONCURRENCY: usize = 8;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct LoadStats {
    pub markets_selected: usize,
    pub markets_unresolved: usize,
    pub markets_too_many_trades: usize,
    pub markets_failed: usize,
    /// Markets whose payouts came from Gamma final prices.
    pub payouts_from_gamma: usize,
    /// Markets where /v2/resolutions payouts and Gamma final prices differ.
    pub payout_source_mismatches: usize,
    pub markets_used: usize,
    pub trades: usize,
    pub wallets: usize,
    pub wallets_failed: usize,
}

struct Selected {
    record: smartmoney_store::MarketRecord,
    control: bool,
}

/// Pick recently ended markets per tag.
async fn select_markets(
    client: &PolymarketClient,
    bt: &BacktestConfig,
    now: i64,
) -> Result<Vec<Selected>, ApiError> {
    // Day granularity keeps the cache key stable for a day.
    let end_max = chrono::DateTime::from_timestamp(now - now % DAY, 0)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let oldest_end = now - bt.lookback_days as i64 * DAY;
    let groups = bt
        .niche_tags
        .iter()
        .map(|t| (t, bt.markets_per_niche_tag, false))
        .chain(
            bt.control_tags
                .iter()
                .map(|t| (t, bt.markets_per_control_tag, true)),
        );

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (tag, quota, control) in groups {
        let mut taken = 0;
        'pages: for page in 0..MAX_EVENT_PAGES {
            let events = client
                .closed_events(tag, &end_max, page * EVENTS_PAGE, EVENTS_PAGE)
                .await?;
            if events.is_empty() {
                break;
            }
            for event in events {
                for m in event.markets {
                    if taken >= quota {
                        break 'pages;
                    }
                    let record = m.into_record(None, now);
                    let ended = record.end_date.unwrap_or(now);
                    let usable = record.closed
                        && record.clob_token_ids.len() >= 2
                        && record.volume_usd.unwrap_or(0.0) >= bt.min_market_volume_usd
                        && ended >= oldest_end
                        && ended <= now;
                    if usable && seen.insert(record.condition_id.clone()) {
                        out.push(Selected { record, control });
                        taken += 1;
                    }
                }
            }
        }
        tracing::info!(%tag, markets = taken, "selected markets");
    }
    Ok(out)
}

/// Where the final payouts came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutSource {
    /// `/v2/resolutions` payouts.
    Payouts,
    /// Gamma final prices; only used when both Gamma and `/v2/resolutions`
    /// report "resolved" and the prices are exactly 0/1 or 0.5/0.5.
    GammaFinalPrices,
}

enum CaseOutcome {
    Ok(Box<MarketCase>, PayoutSource, bool),
    Unresolved,
    TooManyTrades,
    Failed(ApiError),
}

/// Final payouts implied by Gamma's prices of a resolved market.
pub fn gamma_payouts(record: &smartmoney_store::MarketRecord) -> Option<Vec<f64>> {
    let resolved = record
        .resolution_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("resolved"));
    let p = &record.outcome_prices;
    let exact = |v: f64| [0.0, 0.5, 1.0].iter().any(|x| (v - x).abs() < 1e-9);
    let sums_to_one = (p.iter().sum::<f64>() - 1.0).abs() < 1e-9;
    (resolved && record.closed && p.len() >= 2 && p.iter().all(|v| exact(*v)) && sums_to_one)
        .then(|| p.clone())
}

/// Combine both sources. Returns payouts, their source, and whether the two
/// sources disagreed (when both were available).
pub fn resolve_payouts(
    row: Option<&smartmoney_collector::api::ResolutionRow>,
    record: &smartmoney_store::MarketRecord,
) -> Option<(Vec<f64>, PayoutSource, bool)> {
    let from_api = row.and_then(|r| r.to_resolution()).map(|r| r.payouts);
    let from_gamma = gamma_payouts(record);
    match (from_api, from_gamma) {
        (Some(api), gamma) => {
            let mismatch = gamma.is_some_and(|g| g != api);
            Some((api, PayoutSource::Payouts, mismatch))
        }
        (None, Some(gamma)) if row.is_some_and(|r| r.status == "resolved") => {
            Some((gamma, PayoutSource::GammaFinalPrices, false))
        }
        _ => None,
    }
}

async fn load_case(client: &PolymarketClient, s: Selected, max_trades: usize) -> CaseOutcome {
    let cid = s.record.condition_id.clone();
    let row = match client.resolution(&cid).await {
        Ok(r) => r,
        Err(e) => return CaseOutcome::Failed(e),
    };
    let Some((payouts, source, mismatch)) = resolve_payouts(row.as_ref(), &s.record) else {
        return CaseOutcome::Unresolved;
    };
    let mut trades = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = match client.market_trades_page(&cid, cursor.as_deref()).await {
            Ok(p) => p,
            Err(e) => return CaseOutcome::Failed(e),
        };
        for t in &page.data {
            let Ok(side) = t.side.parse::<Side>() else {
                continue;
            };
            trades.push(HistTrade {
                wallet: t.proxy_wallet.to_lowercase(),
                side,
                token_id: t.token_id.clone(),
                outcome_index: t.outcome_index.filter(|i| *i != 999),
                price: t.price,
                size: t.size,
                timestamp: t.timestamp,
            });
        }
        if trades.len() > max_trades {
            return CaseOutcome::TooManyTrades;
        }
        match page.next_cursor() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    trades.sort_by_key(|t| t.timestamp);
    CaseOutcome::Ok(
        Box::new(MarketCase {
            market: s.record,
            payouts,
            trades,
            control: s.control,
        }),
        source,
        mismatch,
    )
}

async fn load_wallet(
    client: &PolymarketClient,
    address: &str,
    first: i64,
    last: i64,
    history_days: u64,
) -> Result<WalletInfo, ApiError> {
    let stats = client.user_stats(address).await?;
    let mut created_at = stats.value.and_then(|s| s.join_date);
    if created_at.is_none() {
        created_at = client.first_activity(address).await?.map(|a| a.timestamp);
    }
    let buys = client
        .user_buys(address, first - history_days as i64 * DAY, last)
        .await?
        .into_iter()
        .map(|t| (t.timestamp, t.condition_id, t.size * t.price))
        .collect();
    Ok(WalletInfo { created_at, buys })
}

pub struct Dataset {
    pub cases: Vec<MarketCase>,
    pub wallets: HashMap<String, WalletInfo>,
    pub stats: LoadStats,
}

pub async fn load(
    client: &PolymarketClient,
    bt: &BacktestConfig,
    now: i64,
) -> Result<Dataset, ApiError> {
    let mut stats = LoadStats::default();
    let selected = select_markets(client, bt, now).await?;
    stats.markets_selected = selected.len();
    tracing::info!(
        markets = selected.len(),
        "loading trades of selected markets"
    );

    let mut cases = Vec::new();
    let mut outcomes = stream::iter(selected)
        .map(|s| load_case(client, s, bt.max_trades_per_market))
        .buffer_unordered(CONCURRENCY);
    let mut done = 0;
    while let Some(outcome) = outcomes.next().await {
        done += 1;
        match outcome {
            CaseOutcome::Ok(c, source, mismatch) => {
                stats.trades += c.trades.len();
                if source == PayoutSource::GammaFinalPrices {
                    stats.payouts_from_gamma += 1;
                }
                if mismatch {
                    stats.payout_source_mismatches += 1;
                    tracing::warn!(condition = %c.market.condition_id, "payouts and Gamma final prices disagree");
                }
                cases.push(*c);
            }
            CaseOutcome::Unresolved => stats.markets_unresolved += 1,
            CaseOutcome::TooManyTrades => stats.markets_too_many_trades += 1,
            CaseOutcome::Failed(e) => {
                tracing::warn!(error = %e, "market skipped");
                stats.markets_failed += 1;
            }
        }
        if done % 50 == 0 {
            tracing::info!(done, total = stats.markets_selected, "markets loaded");
        }
    }
    // Deterministic order regardless of fetch timing.
    cases.sort_by(|a, b| a.market.condition_id.cmp(&b.market.condition_id));
    stats.markets_used = cases.len();

    let candidates = candidate_wallets(&cases, bt);
    tracing::info!(wallets = candidates.len(), "loading wallet facts");
    let mut wallets = HashMap::new();
    let mut results = stream::iter(candidates)
        .map(|(address, (first, last))| async move {
            let r = load_wallet(client, &address, first, last, bt.wallet_history_days).await;
            (address, r)
        })
        .buffer_unordered(CONCURRENCY);
    let mut done = 0;
    while let Some((address, r)) = results.next().await {
        done += 1;
        match r {
            Ok(info) => {
                wallets.insert(address, info);
            }
            Err(e) => {
                tracing::warn!(%address, error = %e, "wallet skipped");
                stats.wallets_failed += 1;
            }
        }
        if done % 200 == 0 {
            tracing::info!(done, "wallets loaded");
        }
    }
    stats.wallets = wallets.len();
    Ok(Dataset {
        cases,
        wallets,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_collector::api::ResolutionRow;
    use smartmoney_core::MarketCategory;
    use smartmoney_store::MarketRecord;

    fn record(prices: Vec<f64>, status: Option<&str>) -> MarketRecord {
        MarketRecord {
            condition_id: "0xm".into(),
            gamma_id: None,
            question: None,
            slug: None,
            event_slug: None,
            category: MarketCategory::Other,
            gamma_category: None,
            tags: vec![],
            volume_usd: None,
            volume_24h_usd: None,
            end_date: None,
            closed: true,
            outcomes: vec![],
            outcome_prices: prices,
            clob_token_ids: vec![],
            neg_risk: None,
            resolution_status: status.map(str::to_owned),
            raw_json: None,
            fetched_at: 0,
        }
    }

    fn row(status: &str, payouts: Option<Vec<i64>>) -> ResolutionRow {
        ResolutionRow {
            condition_id: None,
            status: status.into(),
            payouts,
            resolved_at: None,
        }
    }

    #[test]
    fn prefers_api_payouts_and_flags_mismatch() {
        let r = row("resolved", Some(vec![0, 1_000_000]));
        let (p, src, mismatch) =
            resolve_payouts(Some(&r), &record(vec![0.0, 1.0], Some("resolved"))).unwrap();
        assert_eq!(p, vec![0.0, 1.0]);
        assert_eq!(src, PayoutSource::Payouts);
        assert!(!mismatch);
        let (_, _, mismatch) =
            resolve_payouts(Some(&r), &record(vec![1.0, 0.0], Some("resolved"))).unwrap();
        assert!(mismatch);
    }

    #[test]
    fn gamma_fallback_needs_both_sources_resolved_and_exact_prices() {
        let r = row("resolved", None);
        let (p, src, _) =
            resolve_payouts(Some(&r), &record(vec![1.0, 0.0], Some("resolved"))).unwrap();
        assert_eq!(p, vec![1.0, 0.0]);
        assert_eq!(src, PayoutSource::GammaFinalPrices);
        // Not final prices.
        assert!(resolve_payouts(Some(&r), &record(vec![0.97, 0.03], Some("resolved"))).is_none());
        // Gamma not resolved.
        assert!(resolve_payouts(Some(&r), &record(vec![1.0, 0.0], Some("proposed"))).is_none());
        // Resolutions endpoint not resolved / missing.
        let pending = row("proposed", None);
        assert!(
            resolve_payouts(Some(&pending), &record(vec![1.0, 0.0], Some("resolved"))).is_none()
        );
        assert!(resolve_payouts(None, &record(vec![1.0, 0.0], Some("resolved"))).is_none());
        // 50/50 split is accepted.
        assert!(resolve_payouts(Some(&r), &record(vec![0.5, 0.5], Some("resolved"))).is_some());
    }
}
