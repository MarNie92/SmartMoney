//! Pure backtest evaluation (no I/O).
//!
//! No lookahead: for a trade at time `t` only data with timestamps `<= t`
//! feeds the score (24h volume, wallet buys, cluster buyers). Only the
//! follow-entry price and the final payout look forward, which is the point
//! of the backtest.

use smartmoney_core::config::{BacktestConfig, ScoreMode, ScoringConfig};
use smartmoney_core::{MarketCategory, Side};
use smartmoney_scorer::model::{is_fresh, score, ScoreInput, TradeFacts};
use smartmoney_store::{MarketRecord, PositionRecord, WalletRecord};
use std::collections::{BTreeMap, HashMap};

const DAY: i64 = 86_400;
/// A follow entry must happen within this time after the target moment.
const MAX_FOLLOW_WAIT: i64 = 2 * DAY;

/// A historical trade (any size, any side).
#[derive(Debug, Clone, PartialEq)]
pub struct HistTrade {
    pub wallet: String,
    pub side: Side,
    pub token_id: String,
    pub outcome_index: Option<i64>,
    pub price: f64,
    /// Shares.
    pub size: f64,
    pub timestamp: i64,
}

impl HistTrade {
    pub fn usd(&self) -> f64 {
        self.size * self.price
    }
}

/// A resolved market with its full trade history (ascending by time).
#[derive(Debug, Clone)]
pub struct MarketCase {
    pub market: MarketRecord,
    /// USDC per share for each outcome.
    pub payouts: Vec<f64>,
    pub trades: Vec<HistTrade>,
    /// Sampled as comparison market (sports, crypto).
    pub control: bool,
}

/// Wallet facts that do not depend on time, plus its buy history.
#[derive(Debug, Clone, Default)]
pub struct WalletInfo {
    pub created_at: Option<i64>,
    /// (timestamp, condition_id, usd), any order.
    pub buys: Vec<(i64, String, f64)>,
}

/// A scoring formula plus trade filter to compare.
#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub mode: ScoreMode,
    pub min_usd: f64,
    /// Also accept trades with at least this potential payout (shares x $1)
    /// at a price of at least `min_payout_price` (excludes dust buys of
    /// practically decided outcomes).
    pub min_payout: Option<f64>,
    pub min_payout_price: f64,
}

impl Variant {
    pub fn accepts(&self, t: &HistTrade) -> bool {
        t.side == Side::Buy
            && (t.usd() >= self.min_usd
                || self
                    .min_payout
                    .is_some_and(|p| t.size >= p && t.price >= self.min_payout_price))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FollowResult {
    /// Entry price including slippage.
    pub entry: f64,
    /// Return per dollar staked after fees (payout / entry - 1).
    pub roi: f64,
    /// Price change from the insider's price to the follow price, in %.
    pub move_pct: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Evaluated {
    pub condition_id: String,
    pub wallet: String,
    pub timestamp: i64,
    pub score: f64,
    pub category: MarketCategory,
    pub control: bool,
    pub price: f64,
    pub usd: f64,
    /// Wallet age at trade time in days, if known.
    pub wallet_age_days: Option<f64>,
    /// Payout per share of the bought outcome (1 = won, 0 = lost).
    pub payout: f64,
    /// One entry per configured follow delay; `None` = no fill possible.
    pub follow: Vec<Option<FollowResult>>,
}

/// Evaluate all accepted trades of all markets for one variant.
pub fn evaluate(
    cases: &[MarketCase],
    wallets: &HashMap<String, WalletInfo>,
    variant: &Variant,
    scoring: &ScoringConfig,
    bt: &BacktestConfig,
    fee_bps: f64,
) -> Vec<Evaluated> {
    let cfg = ScoringConfig {
        mode: variant.mode,
        ..scoring.clone()
    };
    let mut out = Vec::new();
    for case in cases {
        let trades = &case.trades;
        let mut window_start = 0usize;
        let mut window_volume = 0.0;
        for (i, t) in trades.iter().enumerate() {
            // Trailing 24h volume (t-24h, t] including all trades up to and
            // including every trade with the same timestamp as `t`.
            window_volume += t.usd();
            while trades[window_start].timestamp <= t.timestamp - DAY {
                window_volume -= trades[window_start].usd();
                window_start += 1;
            }
            if !variant.accepts(t) {
                continue;
            }
            let Some(payout) = t
                .outcome_index
                .and_then(|o| case.payouts.get(o as usize))
                .copied()
            else {
                continue;
            };
            let Some(info) = wallets.get(&t.wallet) else {
                continue;
            };
            let same_time_volume: f64 = trades[i + 1..]
                .iter()
                .take_while(|n| n.timestamp == t.timestamp)
                .map(HistTrade::usd)
                .sum();

            let mut market = case.market.clone();
            market.volume_24h_usd = Some(window_volume + same_time_volume);
            let wallet = wallet_at(&t.wallet, info);
            let positions = positions_at(info, t.timestamp, bt.wallet_history_days);
            let fresh_cluster = fresh_buyers(case, i, variant, wallets, &cfg);
            let facts = TradeFacts {
                side: t.side,
                condition_id: case.market.condition_id.clone(),
                price: t.price,
                usdc_value: t.usd(),
                timestamp: t.timestamp,
            };
            let breakdown = score(
                &ScoreInput {
                    trade: &facts,
                    wallet: Some(&wallet),
                    market: Some(&market),
                    positions: &positions,
                    fresh_cluster_wallets: fresh_cluster,
                },
                &cfg,
            );
            let follow = bt
                .follow_delays_minutes
                .iter()
                .map(|d| follow_entry(trades, i, *d, payout, bt.slippage_bps, fee_bps))
                .collect();
            out.push(Evaluated {
                condition_id: case.market.condition_id.clone(),
                wallet: t.wallet.clone(),
                timestamp: t.timestamp,
                score: breakdown.total,
                category: case.market.category,
                control: case.control,
                price: t.price,
                usd: t.usd(),
                wallet_age_days: info
                    .created_at
                    .map(|c| ((t.timestamp - c) as f64 / DAY as f64).max(0.0)),
                payout,
                follow,
            });
        }
    }
    out
}

/// Wallet record as known at trade time: the creation date is fixed; the
/// number of markets traded is only available as of today and is left out
/// to avoid lookahead.
fn wallet_at(address: &str, info: &WalletInfo) -> WalletRecord {
    WalletRecord {
        address: address.to_owned(),
        join_date: info.created_at,
        markets_traded: None,
        ..Default::default()
    }
}

/// Approximate open positions at `t`: buy volume per market within the
/// trailing history window, up to and including `t`.
fn positions_at(info: &WalletInfo, t: i64, history_days: u64) -> Vec<PositionRecord> {
    let from = t - history_days as i64 * DAY;
    let mut per_market: BTreeMap<&str, f64> = BTreeMap::new();
    for (ts, cid, usd) in &info.buys {
        if *ts >= from && *ts <= t {
            *per_market.entry(cid.as_str()).or_default() += usd;
        }
    }
    per_market
        .into_iter()
        .map(|(cid, usd)| PositionRecord {
            token_id: String::new(),
            condition_id: cid.to_owned(),
            current_size: None,
            current_value: Some(usd),
            total_cost: Some(usd),
        })
        .collect()
}

/// Fresh wallets that bought the same token (passing the variant filter)
/// within the cluster window ending at trade `i`, including its own wallet.
fn fresh_buyers(
    case: &MarketCase,
    i: usize,
    variant: &Variant,
    wallets: &HashMap<String, WalletInfo>,
    cfg: &ScoringConfig,
) -> usize {
    let t = &case.trades[i];
    let from = t.timestamp - (cfg.cluster_window_minutes * 60) as i64;
    let mut first_buy: HashMap<&str, i64> = HashMap::new();
    for other in case.trades[..=i].iter().rev() {
        if other.timestamp < from {
            break;
        }
        if other.token_id == t.token_id && variant.accepts(other) {
            first_buy.insert(other.wallet.as_str(), other.timestamp);
        }
    }
    first_buy
        .into_iter()
        .filter(|(w, ts)| {
            wallets
                .get(*w)
                .is_some_and(|info| is_fresh(Some(&wallet_at(w, info)), *ts, cfg))
        })
        .count()
}

/// Follow the trade at index `i` after `delay_minutes`: entry at the first
/// later trade of the same token at or after the target time.
fn follow_entry(
    trades: &[HistTrade],
    i: usize,
    delay_minutes: u64,
    payout: f64,
    slippage_bps: f64,
    fee_bps: f64,
) -> Option<FollowResult> {
    let insider = &trades[i];
    let target = insider.timestamp + (delay_minutes * 60) as i64;
    let next = trades[i + 1..]
        .iter()
        .find(|n| n.token_id == insider.token_id && n.timestamp >= target)?;
    if next.timestamp - target > MAX_FOLLOW_WAIT {
        return None;
    }
    let entry = next.price * (1.0 + slippage_bps / 10_000.0);
    if !(0.0..0.999).contains(&entry) || entry <= 0.0 {
        return None;
    }
    let fee = fee_bps / 10_000.0;
    Some(FollowResult {
        entry,
        roi: payout * (1.0 - fee) / entry - 1.0,
        move_pct: (next.price / insider.price - 1.0) * 100.0,
    })
}

/// Distinct wallets needed for a set of cases (candidate trades).
pub fn candidate_wallets(cases: &[MarketCase], bt: &BacktestConfig) -> HashMap<String, (i64, i64)> {
    let mut out: HashMap<String, (i64, i64)> = HashMap::new();
    for case in cases {
        for t in &case.trades {
            let candidate = t.side == Side::Buy
                && (t.usd() >= bt.min_candidate_usd || t.size >= bt.variant_min_payout_usd);
            if !candidate {
                continue;
            }
            let e = out
                .entry(t.wallet.clone())
                .or_insert((t.timestamp, t.timestamp));
            e.0 = e.0.min(t.timestamp);
            e.1 = e.1.max(t.timestamp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_790_000_000;

    fn bt() -> BacktestConfig {
        BacktestConfig {
            follow_delays_minutes: vec![0, 15],
            slippage_bps: 0.0,
            ..Default::default()
        }
    }

    fn trade(wallet: &str, side: Side, token: &str, price: f64, usd: f64, ts: i64) -> HistTrade {
        HistTrade {
            wallet: wallet.into(),
            side,
            token_id: token.into(),
            outcome_index: Some(if token == "yes" { 0 } else { 1 }),
            price,
            size: usd / price,
            timestamp: ts,
        }
    }

    fn market(category: MarketCategory) -> MarketRecord {
        MarketRecord {
            condition_id: "0xm".into(),
            gamma_id: None,
            question: None,
            slug: None,
            event_slug: None,
            category,
            gamma_category: None,
            tags: vec![],
            volume_usd: Some(1e9), // lifetime volume must not be used
            volume_24h_usd: Some(1e9),
            end_date: None,
            closed: true,
            outcomes: vec![],
            outcome_prices: vec![],
            clob_token_ids: vec![],
            neg_risk: None,
            resolution_status: None,
            raw_json: None,
            fetched_at: 0,
        }
    }

    fn case(trades: Vec<HistTrade>) -> MarketCase {
        MarketCase {
            market: market(MarketCategory::Personnel),
            payouts: vec![1.0, 0.0],
            trades,
            control: false,
        }
    }

    fn additive() -> Variant {
        Variant {
            name: "a".into(),
            mode: ScoreMode::Additive,
            min_usd: 2000.0,
            min_payout: None,
            min_payout_price: 0.0,
        }
    }

    fn wallets(list: &[(&str, i64)]) -> HashMap<String, WalletInfo> {
        list.iter()
            .map(|(w, age_days)| {
                (
                    w.to_string(),
                    WalletInfo {
                        created_at: Some(T0 - age_days * DAY),
                        buys: vec![],
                    },
                )
            })
            .collect()
    }

    fn run(cases: &[MarketCase], w: &HashMap<String, WalletInfo>, v: &Variant) -> Vec<Evaluated> {
        evaluate(cases, w, v, &ScoringConfig::default(), &bt(), 0.0)
    }

    #[test]
    fn volume_uses_only_past_24h() {
        let trades = vec![
            trade("old", Side::Buy, "yes", 0.5, 50_000.0, T0 - 2 * DAY), // outside window
            trade("x", Side::Sell, "no", 0.5, 18_000.0, T0 - 3600),
            trade("ins", Side::Buy, "yes", 0.5, 2_000.0, T0),
            trade("late", Side::Buy, "yes", 0.5, 100_000.0, T0 + 60), // future
        ];
        let w = wallets(&[("ins", 500), ("old", 500), ("late", 500)]);
        let e = run(&[case(trades)], &w, &additive());
        let ins = e.iter().find(|x| x.wallet == "ins").unwrap();
        // 2,000 of 20,000 (24h) = 10 % -> full 20 points; asym 0; conc 15;
        // personnel 15; old wallet 0.
        assert_eq!(ins.score, 50.0);
    }

    #[test]
    fn same_timestamp_trades_count_for_volume() {
        let trades = vec![
            trade("ins", Side::Buy, "yes", 0.5, 2_000.0, T0),
            trade("x", Side::Sell, "no", 0.5, 38_000.0, T0),
        ];
        let w = wallets(&[("ins", 500)]);
        let e = run(&[case(trades)], &w, &additive());
        // 2,000 of 40,000 = 5 % -> 10 points.
        assert_eq!(e[0].score, 40.0);
    }

    #[test]
    fn cluster_ignores_later_buyers() {
        let trades = vec![
            trade("a", Side::Buy, "yes", 0.5, 2_000.0, T0),
            trade("b", Side::Buy, "yes", 0.5, 2_000.0, T0 + 600),
        ];
        let w = wallets(&[("a", 1), ("b", 1)]);
        let e = run(&[case(trades)], &w, &additive());
        let a = e.iter().find(|x| x.wallet == "a").unwrap();
        let b = e.iter().find(|x| x.wallet == "b").unwrap();
        assert!(b.score - a.score >= 9.9, "b sees a, a must not see b");
    }

    #[test]
    fn wallet_history_after_trade_is_ignored() {
        let mut w = wallets(&[("ins", 500)]);
        w.get_mut("ins").unwrap().buys = vec![
            (T0, "0xm".into(), 2_000.0),
            (T0 + 3600, "0xother".into(), 1_000_000.0), // future, ignore
        ];
        let trades = vec![
            trade("x", Side::Sell, "no", 0.5, 18_000.0, T0 - 60),
            trade("ins", Side::Buy, "yes", 0.5, 2_000.0, T0),
        ];
        let e = run(&[case(trades)], &w, &additive());
        // Still 100 % concentrated -> 15 points included in 50.
        assert_eq!(e[0].score, 50.0);
    }

    #[test]
    fn follow_uses_next_trade_after_delay() {
        let trades = vec![
            trade("ins", Side::Buy, "yes", 0.20, 2_000.0, T0),
            trade("n1", Side::Sell, "yes", 0.25, 100.0, T0 + 60),
            trade("n2", Side::Sell, "no", 0.10, 100.0, T0 + 20 * 60), // other token
            trade("n3", Side::Buy, "yes", 0.40, 100.0, T0 + 30 * 60),
        ];
        let w = wallets(&[("ins", 500)]);
        let e = run(&[case(trades)], &w, &additive());
        let f0 = e[0].follow[0].as_ref().unwrap();
        assert!((f0.entry - 0.25).abs() < 1e-9);
        assert!((f0.roi - 3.0).abs() < 1e-9); // 1 / 0.25 - 1
        assert!((f0.move_pct - 25.0).abs() < 1e-9);
        let f15 = e[0].follow[1].as_ref().unwrap();
        assert!((f15.entry - 0.40).abs() < 1e-9, "skips other token");
        assert_eq!(e[0].payout, 1.0);
    }

    #[test]
    fn follow_none_without_later_trade_and_fees_apply() {
        let trades = vec![trade("ins", Side::Buy, "no", 0.5, 2_000.0, T0)];
        let w = wallets(&[("ins", 500)]);
        let e = run(&[case(trades.clone())], &w, &additive());
        assert_eq!(e[0].payout, 0.0, "bought the losing side");
        assert!(e[0].follow.iter().all(Option::is_none));

        let mut trades = trades;
        trades.push(trade("n", Side::Buy, "no", 0.5, 100.0, T0 + 60));
        let e = evaluate(
            &[case(trades)],
            &w,
            &additive(),
            &ScoringConfig::default(),
            &bt(),
            100.0,
        );
        let f = e[0].follow[0].as_ref().unwrap();
        assert!((f.roi + 1.0).abs() < 1e-9, "lost: -100 %");
    }

    #[test]
    fn variant_filters() {
        let small_longshot = trade("w", Side::Buy, "yes", 0.04, 800.0, T0); // 20k shares
        let sell = trade("w", Side::Sell, "yes", 0.5, 5_000.0, T0);
        let payout = Variant {
            name: "p".into(),
            mode: ScoreMode::Multiplicative,
            min_usd: 2000.0,
            min_payout: Some(15_000.0),
            min_payout_price: 0.02,
        };
        let dust = trade("w", Side::Buy, "yes", 0.001, 20.0, T0); // 20k shares at 0.1¢
        assert!(!payout.accepts(&dust));
        assert!(!additive().accepts(&small_longshot));
        assert!(payout.accepts(&small_longshot));
        assert!(!payout.accepts(&sell));
    }

    #[test]
    fn skips_trades_without_wallet_info_or_outcome() {
        let mut t = trade("ins", Side::Buy, "yes", 0.5, 2_000.0, T0);
        let mut c = case(vec![t.clone()]);
        assert!(run(&[c.clone()], &HashMap::new(), &additive()).is_empty());
        t.outcome_index = None;
        c.trades = vec![t];
        assert!(run(&[c], &wallets(&[("ins", 1)]), &additive()).is_empty());
    }

    #[test]
    fn candidate_wallet_ranges() {
        let c = case(vec![
            trade("a", Side::Buy, "yes", 0.5, 600.0, T0),
            trade("a", Side::Buy, "yes", 0.5, 600.0, T0 + 100),
            trade("b", Side::Buy, "yes", 0.5, 100.0, T0), // too small
            trade("c", Side::Sell, "yes", 0.5, 9_000.0, T0),
        ]);
        let w = candidate_wallets(&[c], &BacktestConfig::default());
        assert_eq!(w.len(), 1);
        assert_eq!(w["a"], (T0, T0 + 100));
    }
}
