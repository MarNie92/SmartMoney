//! Smart-money persistence test (pure, no I/O).
//!
//! Question: do wallets that beat the market in an earlier period keep doing
//! so later? Wallets are ranked by their edge in period 1 using only markets
//! that had already ended (plus a safety margin) before the split, so the
//! ranking never uses an outcome that was unknown at the split. Their
//! period-2 trades (at or after the split) are then evaluated.
//!
//! Per trade edge, in probability points:
//! - BUY:  payout - price  (bought below final value)
//! - SELL: price - payout  (sold above final value)
//!
//! A wallet's edge is the USDC-weighted mean of its trade edges.

use crate::engine::{HistTrade, MarketCase};
use serde::Serialize;
use smartmoney_core::Side;
use std::collections::{HashMap, HashSet};

const DAY: i64 = 86_400;

#[derive(Debug, Clone)]
pub struct SmartConfig {
    /// Period boundary (epoch seconds).
    pub split_ts: i64,
    /// Period-1 markets must have ended at or after this time.
    pub p1_start_ts: i64,
    /// Period-2 trades must be before this time.
    pub p2_end_ts: i64,
    /// Period-1 markets must have ended this many days before the split.
    pub resolve_margin_days: i64,
    pub p1_min_trades: usize,
    pub p1_min_markets: usize,
    pub p2_min_trades: usize,
    /// Number of ranked groups (5 = quintiles).
    pub groups: usize,
    /// Copy test: follow period-2 trades of at least this size. Sells are
    /// followed by buying the opposite outcome (binary markets only).
    pub copy_min_usd: f64,
    pub follow_delays_minutes: Vec<u64>,
    pub slippage_bps: f64,
    pub fee_bps: f64,
}

#[derive(Debug, Default, Clone)]
struct Acc {
    usd: f64,
    edge_usd: f64,
    trades: usize,
    markets: HashSet<String>,
}

impl Acc {
    fn add(&mut self, usd: f64, edge: f64, market: &str) {
        self.usd += usd;
        self.edge_usd += usd * edge;
        self.trades += 1;
        self.markets.insert(market.to_owned());
    }

    fn edge(&self) -> f64 {
        if self.usd > 0.0 {
            self.edge_usd / self.usd
        } else {
            0.0
        }
    }
}

/// Edge of one trade at resolution, if its outcome payout is known.
pub fn trade_edge(t: &HistTrade, payouts: &[f64]) -> Option<f64> {
    let payout = *payouts.get(t.outcome_index? as usize)?;
    Some(match t.side {
        Side::Buy => payout - t.price,
        Side::Sell => t.price - payout,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CopyStats {
    pub delay_minutes: u64,
    pub filled: usize,
    pub avg_roi: Option<f64>,
    pub markets: usize,
    pub buy_filled: usize,
    pub buy_avg_roi: Option<f64>,
    pub sell_filled: usize,
    pub sell_avg_roi: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkillGroup {
    /// 1 = best period-1 edge.
    pub rank: usize,
    pub wallets: usize,
    pub p1_edge_avg: f64,
    /// Mean of the wallets' period-2 edges (each wallet counts once).
    pub p2_edge_avg: f64,
    /// 95 % interval of `p2_edge_avg` across wallets.
    pub p2_edge_lo: f64,
    pub p2_edge_hi: f64,
    /// Period-2 edge weighted by traded USDC across the group.
    pub p2_edge_usd_weighted: f64,
    pub p2_trades: usize,
    pub p2_markets: usize,
    pub copy: Vec<CopyStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SmartReport {
    pub eligible_wallets: usize,
    pub p1_markets: usize,
    pub p2_markets: usize,
    /// Spearman rank correlation of period-1 vs period-2 wallet edges.
    pub spearman: Option<f64>,
    pub groups: Vec<SkillGroup>,
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
    let mut out = vec![0.0; values.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && values[idx[j + 1]] == values[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0;
        for k in &idx[i..=j] {
            out[*k] = avg;
        }
        i = j + 1;
    }
    out
}

pub fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 3 {
        return None;
    }
    let (ra, rb) = (ranks(a), ranks(b));
    let n = a.len() as f64;
    let ma = ra.iter().sum::<f64>() / n;
    let mb = rb.iter().sum::<f64>() / n;
    let cov: f64 = ra.iter().zip(&rb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f64 = ra.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = rb.iter().map(|y| (y - mb).powi(2)).sum();
    (va > 0.0 && vb > 0.0).then(|| cov / (va * vb).sqrt())
}

/// Follow trade `i` after a delay by buying `outcome`. The entry price comes
/// from the next trade in the market after the target time; in a binary
/// market a trade of the other outcome at price p implies 1 - p.
fn follow_roi(
    trades: &[HistTrade],
    i: usize,
    delay: u64,
    outcome: i64,
    binary: bool,
    payout: f64,
    cfg: &SmartConfig,
) -> Option<f64> {
    let t = &trades[i];
    let target = t.timestamp + (delay * 60) as i64;
    let next = trades[i + 1..].iter().find(|n| {
        n.timestamp >= target
            && n.outcome_index.is_some()
            && (binary || n.outcome_index == Some(outcome))
    })?;
    if next.timestamp - target > 2 * DAY {
        return None;
    }
    let price = if next.outcome_index == Some(outcome) {
        next.price
    } else {
        1.0 - next.price
    };
    let entry = price * (1.0 + cfg.slippage_bps / 10_000.0);
    if entry <= 0.0 || entry >= 0.999 {
        return None;
    }
    Some(payout * (1.0 - cfg.fee_bps / 10_000.0) / entry - 1.0)
}

pub fn evaluate(cases: &[MarketCase], cfg: &SmartConfig) -> SmartReport {
    let p1_cutoff = cfg.split_ts - cfg.resolve_margin_days * DAY;
    let mut p1: HashMap<&str, Acc> = HashMap::new();
    let mut p2: HashMap<&str, Acc> = HashMap::new();
    let mut p1_markets = HashSet::new();
    let mut p2_markets = HashSet::new();

    for case in cases {
        let cid = case.market.condition_id.as_str();
        let ended_before_split = case
            .market
            .end_date
            .is_some_and(|e| e < p1_cutoff && e >= cfg.p1_start_ts);
        for t in &case.trades {
            let Some(edge) = trade_edge(t, &case.payouts) else {
                continue;
            };
            if t.timestamp < cfg.split_ts {
                if ended_before_split {
                    p1.entry(&t.wallet).or_default().add(t.usd(), edge, cid);
                    p1_markets.insert(cid);
                }
            } else if t.timestamp < cfg.p2_end_ts {
                p2.entry(&t.wallet).or_default().add(t.usd(), edge, cid);
                p2_markets.insert(cid);
            }
        }
    }

    // Eligible wallets, sorted by period-1 edge (best first).
    let mut eligible: Vec<(&str, f64, f64)> = p1
        .iter()
        .filter(|(_, a)| a.trades >= cfg.p1_min_trades && a.markets.len() >= cfg.p1_min_markets)
        .filter_map(|(w, a1)| {
            let a2 = p2.get(w)?;
            (a2.trades >= cfg.p2_min_trades).then(|| (*w, a1.edge(), a2.edge()))
        })
        .collect();
    eligible.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));

    let spearman = spearman(
        &eligible.iter().map(|e| e.1).collect::<Vec<_>>(),
        &eligible.iter().map(|e| e.2).collect::<Vec<_>>(),
    );

    let groups_n = cfg.groups.max(1);
    let mut groups = Vec::new();
    if !eligible.is_empty() {
        for g in 0..groups_n {
            let from = g * eligible.len() / groups_n;
            let to = (g + 1) * eligible.len() / groups_n;
            let members = &eligible[from..to];
            if members.is_empty() {
                continue;
            }
            let wallets: HashSet<&str> = members.iter().map(|m| m.0).collect();
            let n = members.len() as f64;
            let p2_mean = members.iter().map(|m| m.2).sum::<f64>() / n;
            let var = if members.len() > 1 {
                members.iter().map(|m| (m.2 - p2_mean).powi(2)).sum::<f64>() / (n - 1.0)
            } else {
                0.0
            };
            let se = (var / n).sqrt();
            let (usd, edge_usd, trades, markets) =
                members
                    .iter()
                    .fold((0.0, 0.0, 0usize, HashSet::new()), |(u, e, t, mut m), w| {
                        let a = &p2[w.0];
                        m.extend(a.markets.iter().cloned());
                        (u + a.usd, e + a.edge_usd, t + a.trades, m)
                    });
            let copy = cfg
                .follow_delays_minutes
                .iter()
                .map(|d| copy_stats(cases, &wallets, cfg, *d))
                .collect();
            groups.push(SkillGroup {
                rank: g + 1,
                wallets: members.len(),
                p1_edge_avg: members.iter().map(|m| m.1).sum::<f64>() / n,
                p2_edge_avg: p2_mean,
                p2_edge_lo: p2_mean - 1.96 * se,
                p2_edge_hi: p2_mean + 1.96 * se,
                p2_edge_usd_weighted: if usd > 0.0 { edge_usd / usd } else { 0.0 },
                p2_trades: trades,
                p2_markets: markets.len(),
                copy,
            });
        }
    }

    SmartReport {
        eligible_wallets: eligible.len(),
        p1_markets: p1_markets.len(),
        p2_markets: p2_markets.len(),
        spearman,
        groups,
    }
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Copy test: follow every period-2 trade (>= copy_min_usd) of the wallets.
/// A BUY is copied as the same buy; a SELL as a buy of the opposite outcome
/// (binary markets only, since only there the opposite is well defined).
fn copy_stats(
    cases: &[MarketCase],
    wallets: &HashSet<&str>,
    cfg: &SmartConfig,
    delay: u64,
) -> CopyStats {
    let mut buys = Vec::new();
    let mut sells = Vec::new();
    let mut markets = HashSet::new();
    for case in cases {
        let binary = case.payouts.len() == 2;
        for (i, t) in case.trades.iter().enumerate() {
            if t.timestamp < cfg.split_ts
                || t.timestamp >= cfg.p2_end_ts
                || t.usd() < cfg.copy_min_usd
                || !wallets.contains(t.wallet.as_str())
            {
                continue;
            }
            let Some(own) = t.outcome_index else {
                continue;
            };
            let outcome = match t.side {
                Side::Buy => own,
                Side::Sell if binary => 1 - own,
                Side::Sell => continue,
            };
            let Some(payout) = case.payouts.get(outcome as usize).copied() else {
                continue;
            };
            if let Some(r) = follow_roi(&case.trades, i, delay, outcome, binary, payout, cfg) {
                match t.side {
                    Side::Buy => buys.push(r),
                    Side::Sell => sells.push(r),
                }
                markets.insert(case.market.condition_id.as_str());
            }
        }
    }
    let all: Vec<f64> = buys.iter().chain(&sells).copied().collect();
    CopyStats {
        delay_minutes: delay,
        filled: all.len(),
        avg_roi: mean(&all),
        markets: markets.len(),
        buy_filled: buys.len(),
        buy_avg_roi: mean(&buys),
        sell_filled: sells.len(),
        sell_avg_roi: mean(&sells),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::MarketCategory;
    use smartmoney_store::MarketRecord;

    const SPLIT: i64 = 1_800_000_000;

    fn cfg() -> SmartConfig {
        SmartConfig {
            split_ts: SPLIT,
            p1_start_ts: i64::MIN,
            p2_end_ts: i64::MAX,
            resolve_margin_days: 3,
            p1_min_trades: 2,
            p1_min_markets: 2,
            p2_min_trades: 1,
            groups: 2,
            copy_min_usd: 0.0,
            follow_delays_minutes: vec![0],
            slippage_bps: 0.0,
            fee_bps: 0.0,
        }
    }

    fn trade(wallet: &str, side: Side, outcome: i64, price: f64, ts: i64) -> HistTrade {
        HistTrade {
            wallet: wallet.into(),
            side,
            token_id: format!("tok{outcome}"),
            outcome_index: Some(outcome),
            price,
            size: 100.0 / price,
            timestamp: ts,
        }
    }

    fn case(id: &str, end: i64, winner: usize, trades: Vec<HistTrade>) -> MarketCase {
        let mut payouts = vec![0.0, 0.0];
        payouts[winner] = 1.0;
        MarketCase {
            market: MarketRecord {
                condition_id: id.into(),
                gamma_id: None,
                question: None,
                slug: None,
                event_slug: None,
                category: MarketCategory::Other,
                gamma_category: None,
                tags: vec![],
                volume_usd: None,
                volume_24h_usd: None,
                end_date: Some(end),
                closed: true,
                outcomes: vec![],
                outcome_prices: vec![],
                clob_token_ids: vec![],
                neg_risk: None,
                resolution_status: None,
                raw_json: None,
                fetched_at: 0,
            },
            payouts,
            trades,
            control: false,
        }
    }

    #[test]
    fn trade_edges() {
        let p = [1.0, 0.0];
        assert_eq!(trade_edge(&trade("w", Side::Buy, 0, 0.3, 0), &p), Some(0.7));
        assert_eq!(
            trade_edge(&trade("w", Side::Buy, 1, 0.3, 0), &p),
            Some(-0.3)
        );
        assert_eq!(
            trade_edge(&trade("w", Side::Sell, 1, 0.3, 0), &p),
            Some(0.3)
        );
    }

    #[test]
    fn spearman_basic() {
        assert!((spearman(&[1.0, 2.0, 3.0], &[10.0, 20.0, 30.0]).unwrap() - 1.0).abs() < 1e-9);
        assert!((spearman(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]).unwrap() + 1.0).abs() < 1e-9);
        assert_eq!(spearman(&[1.0, 2.0], &[1.0, 2.0]), None);
    }

    #[test]
    fn ranking_ignores_markets_not_resolved_before_split() {
        let day = DAY;
        let cases = vec![
            // Ended well before the split: counts for period 1.
            case(
                "m1",
                SPLIT - 10 * day,
                0,
                vec![
                    trade("good", Side::Buy, 0, 0.2, SPLIT - 20 * day),
                    trade("bad", Side::Buy, 1, 0.8, SPLIT - 20 * day),
                ],
            ),
            case(
                "m2",
                SPLIT - 9 * day,
                1,
                vec![
                    trade("good", Side::Buy, 1, 0.3, SPLIT - 19 * day),
                    trade("bad", Side::Buy, 0, 0.7, SPLIT - 19 * day),
                ],
            ),
            // Ends only 1 day before the split (inside the margin): its
            // outcome must not influence the ranking.
            case(
                "m3",
                SPLIT - day,
                0,
                vec![
                    trade("bad", Side::Buy, 0, 0.01, SPLIT - 2 * day),
                    trade("bad", Side::Buy, 0, 0.01, SPLIT - 2 * day),
                ],
            ),
            // Period 2: the good wallet wins again.
            case(
                "m4",
                SPLIT + 10 * day,
                0,
                vec![
                    trade("good", Side::Buy, 0, 0.4, SPLIT + day),
                    trade("bad", Side::Buy, 1, 0.6, SPLIT + day),
                    trade("x", Side::Sell, 0, 0.5, SPLIT + day + 60),
                ],
            ),
        ];
        let r = evaluate(&cases, &cfg());
        assert_eq!(r.eligible_wallets, 2);
        assert_eq!(r.p1_markets, 2, "m3 excluded from period 1");
        let top = &r.groups[0];
        assert_eq!(top.rank, 1);
        assert!(
            top.p1_edge_avg > 0.7,
            "good wallet ranked first: {}",
            top.p1_edge_avg
        );
        assert!((top.p2_edge_avg - 0.6).abs() < 1e-9);
        let bottom = &r.groups[1];
        assert!(bottom.p2_edge_avg < 0.0);
        // Copy: good wallet's period-2 buy at 0.4, next trade on tok0 at 0.5.
        let c = &top.copy[0];
        assert_eq!(c.filled, 1);
        // Next trade is the bad wallet buying outcome 1 at 0.6 -> outcome 0
        // implied at 0.4 -> 1 / 0.4 - 1.
        assert!((c.avg_roi.unwrap() - 1.5).abs() < 1e-9);
        assert_eq!(c.buy_filled, 1);
        assert_eq!(c.sell_filled, 0);
    }

    #[test]
    fn eligibility_thresholds_apply() {
        let cases = vec![case(
            "m1",
            SPLIT - 10 * DAY,
            0,
            vec![
                trade("w", Side::Buy, 0, 0.5, SPLIT - 20 * DAY),
                trade("w", Side::Buy, 0, 0.5, SPLIT + DAY),
            ],
        )];
        // Only one market in period 1 -> not eligible.
        let r = evaluate(&cases, &cfg());
        assert_eq!(r.eligible_wallets, 0);
        assert!(r.groups.is_empty());
    }
    #[test]
    fn sells_are_copied_as_opposite_buys() {
        let cases = vec![
            case(
                "m1",
                SPLIT - 10 * DAY,
                1,
                vec![trade("w", Side::Sell, 0, 0.7, SPLIT - 20 * DAY)],
            ),
            case(
                "m2",
                SPLIT - 9 * DAY,
                1,
                vec![trade("w", Side::Sell, 0, 0.6, SPLIT - 19 * DAY)],
            ),
            case(
                "m3",
                SPLIT + 10 * DAY,
                1,
                vec![
                    trade("w", Side::Sell, 0, 0.7, SPLIT + DAY),
                    trade("x", Side::Buy, 1, 0.35, SPLIT + DAY + 60),
                ],
            ),
        ];
        let mut c = cfg();
        c.groups = 1;
        let r = evaluate(&cases, &c);
        assert_eq!(r.eligible_wallets, 1);
        let g = &r.groups[0];
        assert!(
            (g.p1_edge_avg - 0.65).abs() < 1e-9,
            "sold losers: {}",
            g.p1_edge_avg
        );
        let copy = &g.copy[0];
        assert_eq!(copy.sell_filled, 1);
        assert!((copy.sell_avg_roi.unwrap() - (1.0 / 0.35 - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn period_windows_limit_markets_and_trades() {
        let cases = vec![
            case(
                "old",
                SPLIT - 100 * DAY,
                0,
                vec![
                    trade("w", Side::Buy, 0, 0.5, SPLIT - 110 * DAY),
                    trade("w", Side::Buy, 0, 0.5, SPLIT - 105 * DAY),
                ],
            ),
            case(
                "m1",
                SPLIT - 10 * DAY,
                0,
                vec![trade("w", Side::Buy, 0, 0.5, SPLIT - 20 * DAY)],
            ),
            case(
                "m2",
                SPLIT + 100 * DAY,
                0,
                vec![trade("w", Side::Buy, 0, 0.5, SPLIT + 95 * DAY)],
            ),
        ];
        let mut c = cfg();
        c.p1_start_ts = SPLIT - 50 * DAY;
        c.p2_end_ts = SPLIT + 90 * DAY;
        let r = evaluate(&cases, &c);
        assert_eq!(r.p1_markets, 1, "old market before the period-1 window");
        assert_eq!(r.p2_markets, 0, "trade after the period-2 window");
        assert_eq!(r.eligible_wallets, 0);
    }
}
