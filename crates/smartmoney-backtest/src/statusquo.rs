//! Status-quo strategy test ("nothing ever happens").
//!
//! Rule: in a Yes/No market asking whether something happens by a deadline,
//! buy NO once, at the first trade where the NO price lies in the entry band
//! and enough time is left until the market ends; hold to resolution.
//!
//! Realistic costs: slippage on the entry price plus Polymarket's taker fee
//! `fee = shares * rate * p * (1 - p)` with the category's fee rate.
//! Uncertainty is clustered by event (deadline ladders like "by March" /
//! "by June" share one outcome path).

use crate::engine::MarketCase;
use serde::Serialize;
use smartmoney_core::MarketCategory;
use smartmoney_store::MarketRecord;
use std::collections::HashMap;

const DAY: i64 = 86_400;

/// Question patterns of deadline markets, where NO is the status quo.
const DEADLINE_PATTERNS: &[&str] = &[
    " by ",
    " before ",
    "by end of",
    "by the end of",
    " in 2025",
    " in 2026",
    " in 2027",
    "this year",
    "this month",
    "this week",
];

/// Taker fee rate per category (docs.polymarket.com/trading/fees.md,
/// 2026-11-10). Geopolitics is fee-free but cannot be told apart from
/// politics here, so the higher rate is used (conservative).
pub fn fee_rate(category: MarketCategory) -> f64 {
    match category {
        MarketCategory::CryptoPrice => 0.07,
        MarketCategory::Sports | MarketCategory::Other => 0.05,
        MarketCategory::Politics
        | MarketCategory::Personnel
        | MarketCategory::Corporate
        | MarketCategory::Legal => 0.04,
    }
}

/// Index of the "No" outcome of a Yes/No market.
pub fn no_index(m: &MarketRecord) -> Option<usize> {
    let names: Vec<String> = m.outcomes.iter().map(|o| o.trim().to_lowercase()).collect();
    (names.len() == 2 && names.contains(&"yes".to_string()))
        .then(|| names.iter().position(|o| o == "no"))
        .flatten()
}

/// Deadline-style Yes/No market outside sports, crypto prices and weather.
pub fn is_status_quo_market(m: &MarketRecord) -> bool {
    if no_index(m).is_none()
        || matches!(
            m.category,
            MarketCategory::Sports | MarketCategory::CryptoPrice
        )
        || m.tags
            .iter()
            .any(|t| t.contains("weather") || t.contains("temperature"))
    {
        return false;
    }
    let q = m.question.as_deref().unwrap_or_default().to_lowercase();
    q.starts_with("will ") && DEADLINE_PATTERNS.iter().any(|p| q.contains(p))
}

#[derive(Debug, Clone)]
pub struct SqRule {
    /// NO price band (inclusive low, exclusive high).
    pub band: (f64, f64),
    /// Minimum days between entry and market end.
    pub min_days: i64,
    /// Minimum size (USDC) of the trade that sets the entry price; tiny
    /// prints at odd prices are not a price one could have traded at.
    pub min_trade_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub condition_id: String,
    pub cluster: String,
    pub entered_at: i64,
    /// Cost per share including slippage and fee.
    pub cost: f64,
    pub payout: f64,
}

impl Entry {
    pub fn roi(&self) -> f64 {
        self.payout / self.cost - 1.0
    }
}

/// First qualifying NO entry of a market under `rule`.
pub fn entry(case: &MarketCase, rule: &SqRule, slippage_bps: f64) -> Option<Entry> {
    let no = no_index(&case.market)?;
    let payout = *case.payouts.get(no)?;
    let end = case.market.end_date?;
    let rate = fee_rate(case.market.category);
    case.trades.iter().find_map(|t| {
        let idx = t.outcome_index? as usize;
        let no_price = if idx == no { t.price } else { 1.0 - t.price };
        let ok = t.usd() >= rule.min_trade_usd
            && no_price >= rule.band.0
            && no_price < rule.band.1
            && end - t.timestamp >= rule.min_days * DAY;
        if !ok {
            return None;
        }
        let price = (no_price * (1.0 + slippage_bps / 10_000.0)).min(0.999);
        let fee = rate * price * (1.0 - price);
        Some(Entry {
            condition_id: case.market.condition_id.clone(),
            cluster: case
                .market
                .event_slug
                .clone()
                .unwrap_or_else(|| case.market.condition_id.clone()),
            entered_at: t.timestamp,
            cost: price + fee,
            payout,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SqResult {
    pub label: String,
    pub markets: usize,
    pub events: usize,
    /// Share of entries whose NO paid out fully.
    pub win_rate: f64,
    pub avg_cost: f64,
    /// Mean return per trade after fees and slippage.
    pub roi: f64,
    /// 95 % interval of `roi`, clustered by event.
    pub roi_lo: f64,
    pub roi_hi: f64,
    /// Total profit with a fixed stake per trade.
    pub pnl: f64,
    /// Largest peak-to-trough fall of cumulative profit (entries in time order).
    pub max_drawdown: f64,
    /// Worst single trade loss with the fixed stake.
    pub worst_trade: f64,
}

pub fn summarize(label: &str, entries: &[Entry], stake: f64) -> Option<SqResult> {
    if entries.is_empty() {
        return None;
    }
    let n = entries.len() as f64;
    let roi = entries.iter().map(Entry::roi).sum::<f64>() / n;
    let mut clusters: HashMap<&str, f64> = HashMap::new();
    for e in entries {
        *clusters.entry(e.cluster.as_str()).or_default() += e.roi() - roi;
    }
    let se = clusters.values().map(|s| s * s).sum::<f64>().sqrt() / n;

    let mut ordered: Vec<&Entry> = entries.iter().collect();
    ordered.sort_by_key(|e| e.entered_at);
    let (mut cum, mut peak, mut dd) = (0.0f64, 0.0f64, 0.0f64);
    for e in &ordered {
        cum += stake * e.roi();
        peak = peak.max(cum);
        dd = dd.max(peak - cum);
    }
    Some(SqResult {
        label: label.to_owned(),
        markets: entries.len(),
        events: clusters.len(),
        win_rate: entries.iter().filter(|e| e.payout >= 0.999).count() as f64 / n,
        avg_cost: entries.iter().map(|e| e.cost).sum::<f64>() / n,
        roi,
        roi_lo: roi - 1.96 * se,
        roi_hi: roi + 1.96 * se,
        pnl: cum,
        max_drawdown: dd,
        worst_trade: entries
            .iter()
            .map(|e| stake * e.roi())
            .fold(f64::INFINITY, f64::min),
    })
}

/// Evaluate rules on status-quo markets and on the other Yes/No markets
/// (control). Returns (status quo, control) rows per rule.
pub fn evaluate(
    cases: &[MarketCase],
    rules: &[SqRule],
    slippage_bps: f64,
    stake: f64,
) -> (Vec<SqResult>, Vec<SqResult>) {
    let mut sq_rows = Vec::new();
    let mut control_rows = Vec::new();
    for rule in rules {
        let label = format!(
            "{:.0}–{:.1}¢, ab {} T.",
            rule.band.0 * 100.0,
            rule.band.1 * 100.0,
            rule.min_days
        )
        .replace(".0¢", "¢");
        let (sq, control): (Vec<Entry>, Vec<Entry>) = cases
            .iter()
            .filter(|c| no_index(&c.market).is_some())
            .filter_map(|c| {
                entry(c, rule, slippage_bps).map(|e| (is_status_quo_market(&c.market), e))
            })
            .fold((Vec::new(), Vec::new()), |(mut a, mut b), (is_sq, e)| {
                if is_sq {
                    a.push(e)
                } else {
                    b.push(e)
                }
                (a, b)
            });
        if let Some(r) = summarize(&label, &sq, stake) {
            sq_rows.push(r);
        }
        if let Some(r) = summarize(&label, &control, stake) {
            control_rows.push(r);
        }
    }
    (sq_rows, control_rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::HistTrade;
    use smartmoney_core::Side;

    fn market(question: &str, category: MarketCategory, event: &str, end: i64) -> MarketRecord {
        MarketRecord {
            condition_id: format!("{question}-{event}"),
            gamma_id: None,
            question: Some(question.into()),
            slug: None,
            event_slug: Some(event.into()),
            category,
            gamma_category: None,
            tags: vec![],
            volume_usd: None,
            volume_24h_usd: None,
            end_date: Some(end),
            closed: true,
            outcomes: vec!["Yes".into(), "No".into()],
            outcome_prices: vec![],
            clob_token_ids: vec![],
            neg_risk: None,
            resolution_status: None,
            raw_json: None,
            fetched_at: 0,
        }
    }

    fn trade(outcome: i64, price: f64, ts: i64) -> HistTrade {
        HistTrade {
            wallet: "w".into(),
            side: Side::Buy,
            token_id: format!("t{outcome}"),
            outcome_index: Some(outcome),
            price,
            size: 100.0,
            timestamp: ts,
        }
    }

    fn case(m: MarketRecord, no_wins: bool, trades: Vec<HistTrade>) -> MarketCase {
        MarketCase {
            market: m,
            payouts: if no_wins {
                vec![0.0, 1.0]
            } else {
                vec![1.0, 0.0]
            },
            trades,
            control: false,
            trades_complete: true,
        }
    }

    #[test]
    fn detects_status_quo_markets() {
        let m = |q: &str, c| market(q, c, "e", 0);
        assert!(is_status_quo_market(&m(
            "Will Iran close the Strait of Hormuz by June 30?",
            MarketCategory::Politics
        )));
        assert!(is_status_quo_market(&m(
            "Will Apple release a foldable iPhone in 2026?",
            MarketCategory::Corporate
        )));
        assert!(!is_status_quo_market(&m(
            "Will the Lakers win on 2026-11-08?",
            MarketCategory::Sports
        )));
        assert!(!is_status_quo_market(&m(
            "Will Bitcoin reach $100k by Friday?",
            MarketCategory::CryptoPrice
        )));
        assert!(!is_status_quo_market(&m(
            "Who will win the election?",
            MarketCategory::Politics
        )));
        let mut multi = m("Will X happen by May?", MarketCategory::Other);
        multi.outcomes = vec!["A".into(), "B".into(), "C".into()];
        assert!(!is_status_quo_market(&multi));
    }

    #[test]
    fn entry_uses_first_trade_in_band_with_time_left_and_costs() {
        let end = 100 * DAY;
        let m = market("Will X happen by June?", MarketCategory::Politics, "e", end);
        let c = case(
            m,
            true,
            vec![
                trade(1, 0.80, 0),       // NO at 80¢: below band
                trade(0, 0.06, DAY),     // YES 6¢ -> NO 94¢: in band
                trade(1, 0.96, 2 * DAY), // later, ignored
            ],
        );
        let rule = SqRule {
            band: (0.90, 0.98),
            min_days: 30,
            min_trade_usd: 0.0,
        };
        let e = entry(&c, &rule, 0.0).unwrap();
        assert_eq!(e.entered_at, DAY);
        let fee = 0.04 * 0.94 * 0.06;
        assert!((e.cost - (0.94 + fee)).abs() < 1e-12);
        assert!((e.roi() - (1.0 / (0.94 + fee) - 1.0)).abs() < 1e-12);

        // Not enough time left.
        let late = SqRule {
            band: (0.90, 0.98),
            min_days: 100,
            min_trade_usd: 0.0,
        };
        assert!(entry(&c, &late, 0.0).is_none());
    }

    #[test]
    fn summary_reports_losses_and_drawdown() {
        let e = |cluster: &str, t: i64, payout: f64| Entry {
            condition_id: format!("{cluster}{t}"),
            cluster: cluster.into(),
            entered_at: t,
            cost: 0.95,
            payout,
        };
        // 19 wins, then one loss.
        let mut entries: Vec<Entry> = (0..19).map(|i| e(&format!("c{i}"), i, 1.0)).collect();
        entries.push(e("c19", 100, 0.0));
        let r = summarize("x", &entries, 100.0).unwrap();
        assert_eq!(r.markets, 20);
        assert!((r.win_rate - 0.95).abs() < 1e-12);
        let win = 100.0 * (1.0 / 0.95 - 1.0);
        assert!((r.pnl - (19.0 * win - 100.0)).abs() < 1e-9);
        assert!((r.max_drawdown - 100.0).abs() < 1e-9);
        assert!((r.worst_trade + 100.0).abs() < 1e-9);
        assert!(r.roi_lo < r.roi && r.roi < r.roi_hi);
    }

    #[test]
    fn evaluate_splits_status_quo_and_control() {
        let end = 100 * DAY;
        let cases = vec![
            case(
                market("Will X happen by June?", MarketCategory::Politics, "a", end),
                true,
                vec![trade(1, 0.93, 0)],
            ),
            case(
                market("Will Y win the vote?", MarketCategory::Politics, "b", end),
                false,
                vec![trade(1, 0.93, 0)],
            ),
        ];
        let rules = vec![SqRule {
            band: (0.90, 0.95),
            min_days: 1,
            min_trade_usd: 0.0,
        }];
        let (sq, control) = evaluate(&cases, &rules, 0.0, 100.0);
        assert_eq!(sq.len(), 1);
        assert_eq!(control.len(), 1);
        assert!(sq[0].roi > 0.0);
        assert!((control[0].roi + 1.0).abs() < 1e-9);
    }
    #[test]
    fn entry_ignores_tiny_trades() {
        let end = 100 * DAY;
        let m = market("Will X happen by June?", MarketCategory::Politics, "e", end);
        let mut dust = trade(1, 0.99, 0);
        dust.size = 1.0; // $0.99 print
        let c = case(m, true, vec![dust, trade(1, 0.93, DAY)]);
        let rule = SqRule {
            band: (0.90, 0.995),
            min_days: 1,
            min_trade_usd: 50.0,
        };
        let e = entry(&c, &rule, 0.0).unwrap();
        assert_eq!(e.entered_at, DAY, "dust print skipped");
    }
}
