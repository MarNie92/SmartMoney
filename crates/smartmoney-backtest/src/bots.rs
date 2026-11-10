//! Who makes money on Polymarket, and how?
//!
//! Groups of wallets (very active "bots" from the market sample, the other
//! active traders, the all-time PnL and volume leaderboards) are profiled by
//! their PnL composition from `/v2/user-stats` `all_time_pnl` and by trading
//! behaviour observed in the market sample.
//!
//! Only documented composite fields are used: `economic_pnl` (all-in
//! result), `realized_market_pnl + realized_combo_pnl` (trading),
//! `realized_lp_pnl`, `unrealized_pnl`, `maker_rebate + taker_rebate`,
//! `sponsored_income` (rewards, yield, referral) and `volume_usdc`. Nullable
//! values mean "unavailable" and are never treated as zero.

use crate::engine::MarketCase;
use serde::Serialize;
use serde_json::Value;
use smartmoney_core::{MarketCategory, Side};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PnlBreakdown {
    pub economic: Option<f64>,
    pub trading_realized: Option<f64>,
    pub lp_realized: Option<f64>,
    pub unrealized: Option<f64>,
    pub rebates: Option<f64>,
    pub sponsored: Option<f64>,
    pub volume_usdc: Option<f64>,
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64()
}

fn sum2(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    Some(a? + b?)
}

impl PnlBreakdown {
    /// Parse `data.all_time_pnl` of a `/v2/user-stats` response.
    pub fn from_user_stats(raw: &Value) -> Option<Self> {
        let p = raw.get("data")?.get("all_time_pnl")?;
        if p.is_null() {
            return None;
        }
        Some(Self {
            economic: num(p, "economic_pnl"),
            trading_realized: sum2(num(p, "realized_market_pnl"), num(p, "realized_combo_pnl")),
            lp_realized: num(p, "realized_lp_pnl"),
            unrealized: num(p, "unrealized_pnl"),
            rebates: sum2(num(p, "maker_rebate"), num(p, "taker_rebate")),
            sponsored: num(p, "sponsored_income"),
            volume_usdc: num(p, "volume_usdc"),
        })
    }
}

/// Trading behaviour of one wallet in the market sample.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Behavior {
    pub trades: usize,
    pub sell_share: f64,
    /// Share of trades at a price of at most 10¢ / at least 90¢.
    pub longshot_share: f64,
    pub favorite_share: f64,
    /// Share of markets in which the wallet both bought and sold.
    pub two_sided_share: f64,
    pub sports_share: f64,
    pub crypto_share: f64,
    pub median_usd: f64,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 0 {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    })
}

pub fn behaviors(cases: &[MarketCase], wallets: &HashSet<String>) -> HashMap<String, Behavior> {
    #[derive(Default)]
    struct Acc {
        trades: usize,
        sells: usize,
        longshot: usize,
        favorite: usize,
        sports: usize,
        crypto: usize,
        usd: Vec<f64>,
        sides: HashMap<usize, (bool, bool)>,
    }
    let mut accs: HashMap<&str, Acc> = HashMap::new();
    for (m, case) in cases.iter().enumerate() {
        for t in &case.trades {
            if !wallets.contains(&t.wallet) {
                continue;
            }
            let a = accs.entry(t.wallet.as_str()).or_default();
            a.trades += 1;
            if t.side == Side::Sell {
                a.sells += 1;
            }
            if t.price <= 0.10 {
                a.longshot += 1;
            }
            if t.price >= 0.90 {
                a.favorite += 1;
            }
            match case.market.category {
                MarketCategory::Sports => a.sports += 1,
                MarketCategory::CryptoPrice => a.crypto += 1,
                _ => {}
            }
            a.usd.push(t.usd());
            let s = a.sides.entry(m).or_default();
            match t.side {
                Side::Buy => s.0 = true,
                Side::Sell => s.1 = true,
            }
        }
    }
    accs.into_iter()
        .map(|(w, a)| {
            let n = a.trades as f64;
            let two_sided = a.sides.values().filter(|(b, s)| *b && *s).count();
            (
                w.to_owned(),
                Behavior {
                    trades: a.trades,
                    sell_share: a.sells as f64 / n,
                    longshot_share: a.longshot as f64 / n,
                    favorite_share: a.favorite as f64 / n,
                    two_sided_share: two_sided as f64 / a.sides.len() as f64,
                    sports_share: a.sports as f64 / n,
                    crypto_share: a.crypto as f64 / n,
                    median_usd: median(a.usd).unwrap_or(0.0),
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupProfile {
    pub label: String,
    pub wallets: usize,
    /// Wallets with an `all_time_pnl` point.
    pub with_pnl: usize,
    /// Share of wallets (with PnL) whose all-in result is positive.
    pub profitable_share: Option<f64>,
    pub median_economic: Option<f64>,
    pub sum_economic: f64,
    pub sum_trading_realized: f64,
    pub sum_lp_realized: f64,
    pub sum_unrealized: f64,
    pub sum_rebates: f64,
    pub sum_sponsored: f64,
    pub sum_volume_usdc: f64,
    /// sum_economic / sum_volume_usdc over wallets having both.
    pub margin: Option<f64>,
    /// Wallets with trades in the market sample.
    pub with_behavior: usize,
    pub behavior: Option<Behavior>,
}

pub fn profile(
    label: &str,
    wallets: &[String],
    pnl: &HashMap<String, PnlBreakdown>,
    behavior: &HashMap<String, Behavior>,
) -> GroupProfile {
    let pnls: Vec<&PnlBreakdown> = wallets.iter().filter_map(|w| pnl.get(w)).collect();
    let economics: Vec<f64> = pnls.iter().filter_map(|p| p.economic).collect();
    let sum = |f: fn(&PnlBreakdown) -> Option<f64>| pnls.iter().filter_map(|p| f(p)).sum::<f64>();
    let (margin_pnl, margin_vol) = pnls
        .iter()
        .filter_map(|p| Some((p.economic?, p.volume_usdc?)))
        .fold((0.0, 0.0), |(a, b), (e, v)| (a + e, b + v));

    let behaviors: Vec<&Behavior> = wallets.iter().filter_map(|w| behavior.get(w)).collect();
    let avg = |f: fn(&Behavior) -> f64| {
        behaviors.iter().map(|b| f(b)).sum::<f64>() / behaviors.len() as f64
    };
    let behavior_avg = (!behaviors.is_empty()).then(|| Behavior {
        trades: behaviors.iter().map(|b| b.trades).sum(),
        sell_share: avg(|b| b.sell_share),
        longshot_share: avg(|b| b.longshot_share),
        favorite_share: avg(|b| b.favorite_share),
        two_sided_share: avg(|b| b.two_sided_share),
        sports_share: avg(|b| b.sports_share),
        crypto_share: avg(|b| b.crypto_share),
        median_usd: median(behaviors.iter().map(|b| b.median_usd).collect()).unwrap_or(0.0),
    });

    GroupProfile {
        label: label.to_owned(),
        wallets: wallets.len(),
        with_pnl: pnls.len(),
        profitable_share: (!economics.is_empty()).then(|| {
            economics.iter().filter(|e| **e > 0.0).count() as f64 / economics.len() as f64
        }),
        median_economic: median(economics.clone()),
        sum_economic: economics.iter().sum(),
        sum_trading_realized: sum(|p| p.trading_realized),
        sum_lp_realized: sum(|p| p.lp_realized),
        sum_unrealized: sum(|p| p.unrealized),
        sum_rebates: sum(|p| p.rebates),
        sum_sponsored: sum(|p| p.sponsored),
        sum_volume_usdc: sum(|p| p.volume_usdc),
        margin: (margin_vol > 0.0).then(|| margin_pnl / margin_vol),
        with_behavior: behaviors.len(),
        behavior: behavior_avg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::HistTrade;
    use serde_json::json;
    use smartmoney_store::MarketRecord;

    #[test]
    fn parses_documented_pnl_fields_and_keeps_nulls() {
        let raw = json!({"data": {"all_time_pnl": {
            "economic_pnl": 120.0, "realized_market_pnl": 80.0, "realized_combo_pnl": 5.0,
            "realized_lp_pnl": 0.0, "unrealized_pnl": 10.0, "maker_rebate": 20.0,
            "taker_rebate": null, "sponsored_income": 5.0, "volume_usdc": 10000.0
        }}});
        let p = PnlBreakdown::from_user_stats(&raw).unwrap();
        assert_eq!(p.economic, Some(120.0));
        assert_eq!(p.trading_realized, Some(85.0));
        assert_eq!(p.rebates, None, "null taker rebate makes the sum unknown");
        assert!(PnlBreakdown::from_user_stats(&json!({"data": {"all_time_pnl": null}})).is_none());
        assert!(PnlBreakdown::from_user_stats(&json!({"data": null})).is_none());
    }

    fn trade(wallet: &str, side: Side, price: f64) -> HistTrade {
        HistTrade {
            wallet: wallet.into(),
            side,
            token_id: "t".into(),
            outcome_index: Some(0),
            price,
            size: 100.0,
            timestamp: 0,
        }
    }

    fn case(category: MarketCategory, trades: Vec<HistTrade>) -> MarketCase {
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
                end_date: None,
                closed: true,
                outcomes: vec![],
                outcome_prices: vec![],
                clob_token_ids: vec![],
                neg_risk: None,
                resolution_status: None,
                raw_json: None,
                fetched_at: 0,
            },
            payouts: vec![1.0, 0.0],
            trades,
            control: false,
            trades_complete: true,
        }
    }

    #[test]
    fn behavior_features() {
        let cases = vec![
            case(
                MarketCategory::Sports,
                vec![trade("b", Side::Buy, 0.95), trade("b", Side::Sell, 0.96)],
            ),
            case(
                MarketCategory::Politics,
                vec![trade("b", Side::Buy, 0.05), trade("x", Side::Buy, 0.5)],
            ),
        ];
        let set = HashSet::from(["b".to_string()]);
        let b = &behaviors(&cases, &set)["b"];
        assert_eq!(b.trades, 3);
        assert!((b.sell_share - 1.0 / 3.0).abs() < 1e-9);
        assert!((b.favorite_share - 2.0 / 3.0).abs() < 1e-9);
        assert!((b.longshot_share - 1.0 / 3.0).abs() < 1e-9);
        assert!((b.two_sided_share - 0.5).abs() < 1e-9);
        assert!((b.sports_share - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn group_profile_aggregates() {
        let pnl = HashMap::from([
            (
                "a".to_string(),
                PnlBreakdown {
                    economic: Some(100.0),
                    volume_usdc: Some(10_000.0),
                    rebates: Some(30.0),
                    ..Default::default()
                },
            ),
            (
                "b".to_string(),
                PnlBreakdown {
                    economic: Some(-50.0),
                    volume_usdc: Some(5_000.0),
                    ..Default::default()
                },
            ),
        ]);
        let wallets = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let g = profile("G", &wallets, &pnl, &HashMap::new());
        assert_eq!(g.wallets, 3);
        assert_eq!(g.with_pnl, 2);
        assert_eq!(g.profitable_share, Some(0.5));
        assert_eq!(g.sum_economic, 50.0);
        assert_eq!(g.sum_rebates, 30.0);
        assert!((g.margin.unwrap() - 50.0 / 15_000.0).abs() < 1e-12);
        assert!(g.behavior.is_none());
    }
}
