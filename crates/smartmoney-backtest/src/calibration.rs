//! Market calibration: does an outcome traded at price p win p of the time?
//!
//! Every trade is one observation (price of the traded outcome, its final
//! payout). Positive edge in a bucket means outcomes at that price won more
//! often than priced (buying there was profitable); negative means they were
//! overpriced. Uncertainty is clustered by market, because trades in the same
//! market share one outcome.

use crate::engine::{HistTrade, MarketCase};
use serde::Serialize;
use smartmoney_core::MarketCategory;
use std::collections::HashMap;

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibStats {
    pub label: String,
    pub trades: usize,
    pub markets: usize,
    pub avg_price: Option<f64>,
    pub hit_rate: Option<f64>,
    /// hit_rate - avg_price (trade weighted).
    pub edge: Option<f64>,
    /// 95 % interval of `edge`, clustered by market.
    pub edge_lo: Option<f64>,
    pub edge_hi: Option<f64>,
    /// Return of buying at these prices and holding to resolution.
    pub buy_roi: Option<f64>,
}

#[derive(Default)]
struct Acc {
    n: usize,
    sum_p: f64,
    sum_y: f64,
    /// Per market: (count, sum of (y - p)).
    per_market: HashMap<usize, (usize, f64)>,
}

impl Acc {
    fn add(&mut self, market: usize, p: f64, y: f64) {
        self.n += 1;
        self.sum_p += p;
        self.sum_y += y;
        let e = self.per_market.entry(market).or_default();
        e.0 += 1;
        e.1 += y - p;
    }

    fn stats(&self, label: &str) -> CalibStats {
        if self.n == 0 {
            return CalibStats {
                label: label.to_owned(),
                trades: 0,
                markets: 0,
                avg_price: None,
                hit_rate: None,
                edge: None,
                edge_lo: None,
                edge_hi: None,
                buy_roi: None,
            };
        }
        let n = self.n as f64;
        let price = self.sum_p / n;
        let hit = self.sum_y / n;
        let edge = hit - price;
        // Cluster-robust standard error of the mean residual.
        let var: f64 = self
            .per_market
            .values()
            .map(|(k, s)| (s - *k as f64 * edge).powi(2))
            .sum();
        let se = var.sqrt() / n;
        CalibStats {
            label: label.to_owned(),
            trades: self.n,
            markets: self.per_market.len(),
            avg_price: Some(price),
            hit_rate: Some(hit),
            edge: Some(edge),
            edge_lo: Some(edge - 1.96 * se),
            edge_hi: Some(edge + 1.96 * se),
            buy_roi: (price > 0.0).then(|| hit / price - 1.0),
        }
    }
}

/// One observation per trade: (market index, price, payout of the traded
/// outcome), for trades with a known outcome and a price strictly in (0, 1).
fn observations(
    cases: &[MarketCase],
) -> impl Iterator<Item = (usize, &MarketCase, &HistTrade, f64)> {
    cases.iter().enumerate().flat_map(|(m, case)| {
        case.trades.iter().filter_map(move |t| {
            let y = *case.payouts.get(t.outcome_index? as usize)?;
            (t.price > 0.0 && t.price < 1.0).then_some((m, case, t, y))
        })
    })
}

fn label_range(lo: f64, hi: f64) -> String {
    format!("{:.0}–{:.0}¢", lo * 100.0, hi * 100.0)
}

/// Calibration per price bucket. `bounds` are inner bucket edges in (0, 1),
/// e.g. [0.02, 0.05, 0.1, ...]; a final "Alle" row is appended.
pub fn by_price(cases: &[MarketCase], bounds: &[f64]) -> Vec<CalibStats> {
    let mut edges = vec![0.0];
    edges.extend_from_slice(bounds);
    edges.push(1.0);
    let mut accs: Vec<Acc> = (0..edges.len() - 1).map(|_| Acc::default()).collect();
    let mut all = Acc::default();
    for (m, _, t, y) in observations(cases) {
        let i = edges
            .windows(2)
            .position(|w| t.price >= w[0] && t.price < w[1]);
        if let Some(i) = i {
            accs[i].add(m, t.price, y);
        }
        all.add(m, t.price, y);
    }
    let mut out: Vec<CalibStats> = accs
        .iter()
        .enumerate()
        .map(|(i, a)| a.stats(&label_range(edges[i], edges[i + 1])))
        .collect();
    out.push(all.stats("Alle"));
    out
}

/// Price zones used in the group breakdowns.
pub const ZONES: [(&str, f64, f64); 3] = [
    ("bis 10¢", 0.0, 0.10),
    ("10–90¢", 0.10, 0.90),
    ("ab 90¢", 0.90, 1.0),
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupCalib {
    pub label: String,
    pub trades: usize,
    pub markets: usize,
    /// One entry per `ZONES` element.
    pub zones: Vec<CalibStats>,
}

fn by_group<K: Ord + Clone>(
    cases: &[MarketCase],
    key: impl Fn(&MarketCase, &HistTrade) -> Option<K>,
    label: impl Fn(&K) -> String,
) -> Vec<GroupCalib> {
    let mut groups: std::collections::BTreeMap<K, (Acc, [Acc; 3])> = Default::default();
    for (m, case, t, y) in observations(cases) {
        let Some(k) = key(case, t) else { continue };
        let entry = groups.entry(k).or_default();
        entry.0.add(m, t.price, y);
        if let Some(z) = ZONES
            .iter()
            .position(|(_, lo, hi)| t.price >= *lo && t.price < *hi)
        {
            entry.1[z].add(m, t.price, y);
        }
    }
    groups
        .iter()
        .map(|(k, (all, zones))| GroupCalib {
            label: label(k),
            trades: all.n,
            markets: all.per_market.len(),
            zones: zones
                .iter()
                .zip(ZONES)
                .map(|(a, (name, _, _))| a.stats(name))
                .collect(),
        })
        .collect()
}

pub fn by_category(cases: &[MarketCase]) -> Vec<GroupCalib> {
    by_group(
        cases,
        |c, _| {
            Some(
                MarketCategory::ALL
                    .iter()
                    .position(|x| *x == c.market.category),
            )
        },
        |k| {
            k.map(|i| crate::report::category_label(MarketCategory::ALL[i]).to_owned())
                .unwrap_or_default()
        },
    )
}

/// Time between the trade and the market's end date.
pub const HORIZONS: [(&str, i64); 5] = [
    ("< 1 Std.", HOUR),
    ("1–24 Std.", DAY),
    ("1–7 Tage", 7 * DAY),
    ("7–30 Tage", 30 * DAY),
    ("> 30 Tage", i64::MAX),
];

pub fn by_horizon(cases: &[MarketCase]) -> Vec<GroupCalib> {
    by_group(
        cases,
        |c, t| {
            let left = c.market.end_date? - t.timestamp;
            HORIZONS.iter().position(|(_, max)| left < *max)
        },
        |i| HORIZONS[*i].0.to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::Side;
    use smartmoney_store::MarketRecord;

    fn trade(outcome: i64, price: f64, ts: i64) -> HistTrade {
        HistTrade {
            wallet: "w".into(),
            side: Side::Buy,
            token_id: format!("t{outcome}"),
            outcome_index: Some(outcome),
            price,
            size: 10.0,
            timestamp: ts,
        }
    }

    fn case(
        winner: usize,
        category: MarketCategory,
        end: i64,
        trades: Vec<HistTrade>,
    ) -> MarketCase {
        let mut payouts = vec![0.0, 0.0];
        payouts[winner] = 1.0;
        MarketCase {
            market: MarketRecord {
                condition_id: "m".into(),
                gamma_id: None,
                question: None,
                slug: None,
                event_slug: None,
                category,
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
            trades_complete: true,
        }
    }

    #[test]
    fn overpriced_longshots_show_negative_edge() {
        // Ten markets: longshot (outcome 1) traded at 10¢, never wins;
        // favourite (outcome 0) traded at 90¢, always wins.
        let cases: Vec<MarketCase> = (0..10)
            .map(|_| {
                case(
                    0,
                    MarketCategory::Politics,
                    1000,
                    vec![trade(1, 0.10, 0), trade(0, 0.90, 0)],
                )
            })
            .collect();
        let b = by_price(&cases, &[0.5]);
        assert_eq!(b.len(), 3);
        let low = &b[0];
        assert_eq!(low.trades, 10);
        assert_eq!(low.markets, 10);
        assert!((low.edge.unwrap() + 0.10).abs() < 1e-9);
        assert!((low.buy_roi.unwrap() + 1.0).abs() < 1e-9);
        let high = &b[1];
        assert!((high.edge.unwrap() - 0.10).abs() < 1e-9);
        assert!((high.buy_roi.unwrap() - (1.0 / 0.9 - 1.0)).abs() < 1e-9);
        assert_eq!(b[2].label, "Alle");
        assert!(b[2].edge.unwrap().abs() < 1e-9, "overall calibrated");
    }

    #[test]
    fn interval_is_clustered_by_market() {
        // One market with 100 identical trades has the uncertainty of one
        // observation, not of 100.
        let one = vec![case(
            0,
            MarketCategory::Other,
            1000,
            vec![trade(1, 0.2, 0); 100],
        )];
        let b = by_price(&one, &[]);
        let all = &b[1];
        assert_eq!(all.trades, 100);
        assert_eq!(all.markets, 1);
        // Single cluster: residual exactly at the mean -> zero width.
        assert!((all.edge_hi.unwrap() - all.edge_lo.unwrap()).abs() < 1e-9);
    }

    #[test]
    fn groups_by_category_and_horizon() {
        let cases = vec![
            case(
                0,
                MarketCategory::Sports,
                10 * DAY,
                vec![trade(0, 0.95, 10 * DAY - 60)],
            ),
            case(
                1,
                MarketCategory::Politics,
                10 * DAY,
                vec![trade(1, 0.05, 0)],
            ),
        ];
        let c = by_category(&cases);
        assert_eq!(c.len(), 2);
        assert!(c
            .iter()
            .any(|g| g.label == "Sport" && g.zones[2].trades == 1));
        let h = by_horizon(&cases);
        assert_eq!(h.iter().find(|g| g.label == "< 1 Std.").unwrap().trades, 1);
        assert_eq!(h.iter().find(|g| g.label == "7–30 Tage").unwrap().trades, 1);
    }

    #[test]
    fn ignores_unknown_outcomes_and_boundary_prices() {
        let mut t = trade(0, 0.5, 0);
        t.outcome_index = None;
        let cases = vec![case(
            0,
            MarketCategory::Other,
            1,
            vec![t, trade(0, 1.0, 0), trade(0, 0.0, 0)],
        )];
        assert_eq!(by_price(&cases, &[]).last().unwrap().trades, 0);
    }
}
