//! Pure insider-score calculation (no I/O), shared by the live scorer and
//! the backtest.
//!
//! Every component is `weight * factor` with `factor` in 0..=1. The sum is
//! capped at 100. Only BUY trades are scored; SELLs are usually exits, not
//! informed entries, and get 0.

use serde::Serialize;
use serde_json::{json, Value};
use smartmoney_core::config::{ScoreMode, ScoringConfig};
use smartmoney_core::{MarketCategory, Side};
use smartmoney_store::{MarketRecord, PositionRecord, WalletRecord};

/// Increment when the formula changes so old scores can be identified.
pub const SCORER_VERSION: i64 = 1;
const DAY: f64 = 86_400.0;

/// The trade being scored.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeFacts {
    pub side: Side,
    pub condition_id: String,
    /// USDC per share of the bought outcome.
    pub price: f64,
    pub usdc_value: f64,
    pub timestamp: i64,
}

/// Everything the score depends on, as known at scoring time.
#[derive(Debug, Clone)]
pub struct ScoreInput<'a> {
    pub trade: &'a TradeFacts,
    /// `None` if the wallet could not be enriched.
    pub wallet: Option<&'a WalletRecord>,
    /// `None` if the market could not be enriched.
    pub market: Option<&'a MarketRecord>,
    pub positions: &'a [PositionRecord],
    /// Number of fresh wallets (including this one, if fresh) that bought the
    /// same outcome within the cluster window.
    pub fresh_cluster_wallets: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScoreBreakdown {
    pub total: f64,
    pub wallet_freshness: f64,
    pub price_asymmetry: f64,
    pub relative_size: f64,
    pub concentration: f64,
    pub market_type: f64,
    pub cluster_bonus: f64,
    /// Inputs and factor per component.
    pub details: Value,
}

/// 1 at or below `full`, 0 at or above `zero`, linear in between.
pub fn fade_down(x: f64, full: f64, zero: f64) -> f64 {
    if x <= full {
        1.0
    } else if x >= zero {
        0.0
    } else {
        (zero - x) / (zero - full)
    }
}

/// 0 at or below `zero`, 1 at or above `full`, linear in between.
pub fn fade_up(x: f64, zero: f64, full: f64) -> f64 {
    1.0 - fade_down(x, zero, full)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Freshness factor (0..=1) of a wallet at time `at`, with details.
pub fn wallet_freshness_factor(
    wallet: Option<&WalletRecord>,
    at: i64,
    cfg: &ScoringConfig,
) -> (f64, Value) {
    let Some(w) = wallet else {
        return (0.0, json!({ "note": "wallet not enriched" }));
    };
    let age_days = w
        .created_at()
        .map(|created| ((at - created) as f64 / DAY).max(0.0));
    let age_factor =
        age_days.map(|d| fade_down(d, cfg.fresh_full_age_days, cfg.fresh_zero_age_days));
    // "Fewer than N markets" gets full points: N-1 is the last full value.
    let markets_factor = w.markets_traded.map(|m| {
        fade_down(
            m as f64,
            cfg.fresh_full_markets.saturating_sub(1) as f64,
            cfg.fresh_zero_markets as f64,
        )
    });
    let (factor, note) = match (age_factor, markets_factor) {
        (None, None) => (1.0, "no history known: treated as new wallet"),
        (a, m) => (
            a.unwrap_or(0.0).max(m.unwrap_or(0.0)),
            "max of age and markets",
        ),
    };
    (
        factor,
        json!({
            "age_days": age_days.map(round2),
            "markets_traded": w.markets_traded,
            "age_factor": age_factor.map(round2),
            "markets_factor": markets_factor.map(round2),
            "note": note,
        }),
    )
}

/// True if the wallet counts as fresh for the cluster bonus.
pub fn is_fresh(wallet: Option<&WalletRecord>, at: i64, cfg: &ScoringConfig) -> bool {
    wallet.is_some() && wallet_freshness_factor(wallet, at, cfg).0 >= 0.999
}

pub fn score(input: &ScoreInput<'_>, cfg: &ScoringConfig) -> ScoreBreakdown {
    let w = &cfg.weights;
    let t = input.trade;

    if t.side != Side::Buy {
        return ScoreBreakdown {
            total: 0.0,
            wallet_freshness: 0.0,
            price_asymmetry: 0.0,
            relative_size: 0.0,
            concentration: 0.0,
            market_type: 0.0,
            cluster_bonus: 0.0,
            details: json!({ "note": "sell trades are not scored" }),
        };
    }

    // Wallet freshness.
    let (fresh_factor, fresh_details) = wallet_freshness_factor(input.wallet, t.timestamp, cfg);

    // Price asymmetry.
    let asym_factor = fade_down(t.price, cfg.asymmetry_full_price, cfg.asymmetry_zero_price);

    // Relative size vs. 24h volume (which should already include this trade).
    let volume_24h = input.market.and_then(|m| m.volume_24h_usd);
    let (size_factor, size_share) = match volume_24h {
        Some(v) if v > 0.0 || t.usdc_value > 0.0 => {
            let share = t.usdc_value / v.max(t.usdc_value);
            ((share / cfg.relative_size_full_share).min(1.0), Some(share))
        }
        _ => (0.0, None),
    };

    // Concentration: value in this market vs. all open positions. The
    // position snapshot can lag behind the trade, so the trade value is a
    // lower bound for this market's exposure.
    let in_market: f64 = input
        .positions
        .iter()
        .filter(|p| p.condition_id == t.condition_id)
        .filter_map(|p| p.current_value)
        .sum();
    let elsewhere: f64 = input
        .positions
        .iter()
        .filter(|p| p.condition_id != t.condition_id)
        .filter_map(|p| p.current_value)
        .sum();
    let exposure = in_market.max(t.usdc_value);
    let conc_share = if exposure + elsewhere > 0.0 {
        exposure / (exposure + elsewhere)
    } else {
        0.0
    };
    let conc_factor = fade_up(
        conc_share,
        cfg.concentration_zero_share,
        cfg.concentration_full_share,
    );

    // Market type.
    let category = input.market.map_or(MarketCategory::Other, |m| m.category);
    let type_factor = cfg.category_factors.get(&category).copied().unwrap_or(0.5);

    // Cluster bonus.
    let cluster = input.fresh_cluster_wallets >= cfg.cluster_min_wallets as usize;

    let cluster_points = if cluster { w.cluster_bonus } else { 0.0 };
    let (
        wallet_freshness,
        price_asymmetry,
        relative_size,
        concentration,
        market_type,
        cluster_bonus,
    ) = match cfg.mode {
        ScoreMode::Additive => (
            round2(w.wallet_freshness * fresh_factor),
            round2(w.price_asymmetry * asym_factor),
            round2(w.relative_size * size_factor),
            round2(w.concentration * conc_factor),
            round2(w.market_type * type_factor),
            round2(cluster_points),
        ),
        ScoreMode::Multiplicative => {
            // A brand-new wallet is always "concentrated"; only count
            // concentration to the extent the wallet has history.
            let conc_effective = conc_factor * (1.0 - fresh_factor);
            let max_base = w.wallet_freshness
                + w.price_asymmetry
                + w.relative_size
                + w.concentration
                + w.cluster_bonus;
            let scale = if max_base > 0.0 {
                100.0 / max_base * type_factor
            } else {
                0.0
            };
            (
                round2(w.wallet_freshness * fresh_factor * scale),
                round2(w.price_asymmetry * asym_factor * scale),
                round2(w.relative_size * size_factor * scale),
                round2(w.concentration * conc_effective * scale),
                0.0,
                round2(cluster_points * scale),
            )
        }
    };
    let total = round2(
        (wallet_freshness
            + price_asymmetry
            + relative_size
            + concentration
            + market_type
            + cluster_bonus)
            .min(100.0),
    );

    ScoreBreakdown {
        total,
        wallet_freshness,
        price_asymmetry,
        relative_size,
        concentration,
        market_type,
        cluster_bonus,
        details: json!({
            "version": SCORER_VERSION,
            "mode": cfg.mode,
            "wallet_freshness": { "factor": round2(fresh_factor), "inputs": fresh_details },
            "price_asymmetry": { "factor": round2(asym_factor), "price": t.price },
            "relative_size": {
                "factor": round2(size_factor),
                "usdc_value": round2(t.usdc_value),
                "volume_24h_usd": volume_24h.map(round2),
                "share": size_share.map(|s| (s * 10_000.0).round() / 10_000.0),
            },
            "concentration": {
                "factor": round2(conc_factor),
                "market_exposure_usd": round2(exposure),
                "other_positions_usd": round2(elsewhere),
                "share": round2(conc_share),
            },
            "market_type": { "factor": type_factor, "category": category.as_str() },
            "cluster_bonus": {
                "fresh_wallets": input.fresh_cluster_wallets,
                "min_wallets": cfg.cluster_min_wallets,
                "window_minutes": cfg.cluster_window_minutes,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn cfg() -> ScoringConfig {
        ScoringConfig::default()
    }

    fn trade(price: f64, usd: f64) -> TradeFacts {
        TradeFacts {
            side: Side::Buy,
            condition_id: "0xm".into(),
            price,
            usdc_value: usd,
            timestamp: NOW,
        }
    }

    fn wallet(age_days: Option<f64>, markets: Option<i64>) -> WalletRecord {
        WalletRecord {
            address: "0xw".into(),
            join_date: age_days.map(|d| NOW - (d * DAY) as i64),
            markets_traded: markets,
            ..Default::default()
        }
    }

    fn market(category: MarketCategory, volume_24h: Option<f64>) -> MarketRecord {
        MarketRecord {
            condition_id: "0xm".into(),
            gamma_id: None,
            question: None,
            slug: None,
            event_slug: None,
            category,
            gamma_category: None,
            tags: vec![],
            volume_usd: None,
            volume_24h_usd: volume_24h,
            end_date: None,
            closed: false,
            outcomes: vec![],
            outcome_prices: vec![],
            clob_token_ids: vec![],
            neg_risk: None,
            resolution_status: None,
            raw_json: None,
            fetched_at: 0,
        }
    }

    fn pos(condition: &str, value: f64) -> PositionRecord {
        PositionRecord {
            token_id: format!("t-{condition}"),
            condition_id: condition.into(),
            current_size: None,
            current_value: Some(value),
            total_cost: None,
        }
    }

    fn run(
        t: &TradeFacts,
        w: Option<&WalletRecord>,
        m: Option<&MarketRecord>,
        p: &[PositionRecord],
        cluster: usize,
    ) -> ScoreBreakdown {
        score(
            &ScoreInput {
                trade: t,
                wallet: w,
                market: m,
                positions: p,
                fresh_cluster_wallets: cluster,
            },
            &cfg(),
        )
    }

    #[test]
    fn fades() {
        assert_eq!(fade_down(0.05, 0.05, 0.20), 1.0);
        assert_eq!(fade_down(0.20, 0.05, 0.20), 0.0);
        assert!((fade_down(0.125, 0.05, 0.20) - 0.5).abs() < 1e-9);
        assert_eq!(fade_up(0.2, 0.2, 0.8), 0.0);
        assert_eq!(fade_up(0.9, 0.2, 0.8), 1.0);
        assert!((fade_up(0.5, 0.2, 0.8) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn textbook_insider_scores_maximum() {
        // New wallet, 3-cent entry, 50 % of daily volume, all-in, personnel
        // market, cluster with another fresh wallet.
        let w = wallet(Some(1.0), Some(1));
        let m = market(MarketCategory::Personnel, Some(10_000.0));
        let s = run(
            &trade(0.03, 5_000.0),
            Some(&w),
            Some(&m),
            &[pos("0xm", 5_000.0)],
            2,
        );
        assert_eq!(s.wallet_freshness, 25.0);
        assert_eq!(s.price_asymmetry, 25.0);
        assert_eq!(s.relative_size, 20.0);
        assert_eq!(s.concentration, 15.0);
        assert_eq!(s.market_type, 15.0);
        assert_eq!(s.cluster_bonus, 10.0);
        assert_eq!(s.total, 100.0, "capped at 100");
    }

    #[test]
    fn veteran_sports_favorite_scores_low() {
        // Old wallet with many markets, buying a 90 % favorite in a liquid
        // sports market, diversified.
        let w = wallet(Some(400.0), Some(300));
        let m = market(MarketCategory::Sports, Some(2_000_000.0));
        let s = run(
            &trade(0.90, 5_000.0),
            Some(&w),
            Some(&m),
            &[pos("0xm", 5_000.0), pos("0xa", 50_000.0)],
            0,
        );
        assert_eq!(s.wallet_freshness, 0.0);
        assert_eq!(s.price_asymmetry, 0.0);
        assert!(s.relative_size < 1.0);
        assert_eq!(s.concentration, 0.0);
        assert_eq!(s.market_type, 1.5);
        assert!(s.total < 5.0, "total {}", s.total);
    }

    #[test]
    fn freshness_uses_age_or_market_count() {
        let c = cfg();
        // Young wallet with many markets: fresh by age.
        assert_eq!(
            wallet_freshness_factor(Some(&wallet(Some(3.0), Some(50))), NOW, &c).0,
            1.0
        );
        // Old wallet with few markets: fresh by market count.
        assert_eq!(
            wallet_freshness_factor(Some(&wallet(Some(300.0), Some(4))), NOW, &c).0,
            1.0
        );
        // Halfway on age (18.5 days between 7 and 30), many markets.
        let f = wallet_freshness_factor(Some(&wallet(Some(18.5), Some(100))), NOW, &c).0;
        assert!((f - 0.5).abs() < 1e-6, "{f}");
        // Exactly 5 markets is just past "fewer than 5".
        let f = wallet_freshness_factor(Some(&wallet(Some(300.0), Some(5))), NOW, &c).0;
        assert!(f < 1.0 && f > 0.9, "{f}");
        // Nothing known about an enriched wallet: new.
        assert_eq!(
            wallet_freshness_factor(Some(&wallet(None, None)), NOW, &c).0,
            1.0
        );
        // Not enriched at all: no points.
        assert_eq!(wallet_freshness_factor(None, NOW, &c).0, 0.0);
    }

    #[test]
    fn freshness_is_measured_at_trade_time() {
        let c = cfg();
        let w = wallet(Some(3.0), Some(100)); // 3 days old at NOW
                                              // Ten days later the same trade time still sees a 3-day-old wallet.
        assert_eq!(wallet_freshness_factor(Some(&w), NOW, &c).0, 1.0);
        assert!(wallet_freshness_factor(Some(&w), NOW + 40 * DAY as i64, &c).0 < 0.01);
    }

    #[test]
    fn asymmetry_fades_between_5_and_20_percent() {
        let m = market(MarketCategory::Other, None);
        let at = |p: f64| run(&trade(p, 2_000.0), None, Some(&m), &[], 0).price_asymmetry;
        assert_eq!(at(0.01), 25.0);
        assert_eq!(at(0.05), 25.0);
        assert_eq!(at(0.125), 12.5);
        assert_eq!(at(0.20), 0.0);
        assert_eq!(at(0.70), 0.0);
    }

    #[test]
    fn relative_size_vs_daily_volume() {
        let at = |usd: f64, vol: Option<f64>| {
            let m = market(MarketCategory::Other, vol);
            run(&trade(0.5, usd), None, Some(&m), &[], 0).relative_size
        };
        assert_eq!(at(5_000.0, Some(100_000.0)), 10.0); // 5 % of volume
        assert_eq!(at(10_000.0, Some(100_000.0)), 20.0); // 10 % -> full
        assert_eq!(at(10_000.0, Some(5_000.0)), 20.0); // stale volume < trade
        assert_eq!(at(10_000.0, None), 0.0); // unknown volume
    }

    #[test]
    fn concentration_counts_trade_when_positions_lag() {
        let m = market(MarketCategory::Other, None);
        let at =
            |p: &[PositionRecord]| run(&trade(0.5, 4_000.0), None, Some(&m), p, 0).concentration;
        // Only this trade known -> 100 % concentrated.
        assert_eq!(at(&[]), 15.0);
        // 4k here vs 4k elsewhere -> 50 % -> half of the 0.2..0.8 range.
        assert_eq!(at(&[pos("0xa", 4_000.0)]), 7.5);
        // Larger existing position in this market counts.
        assert_eq!(at(&[pos("0xm", 16_000.0), pos("0xa", 4_000.0)]), 15.0);
        // Diversified.
        assert_eq!(at(&[pos("0xa", 100_000.0)]), 0.0);
    }

    #[test]
    fn market_type_factors_and_missing_market() {
        let at = |c: MarketCategory| {
            let m = market(c, None);
            run(&trade(0.5, 2_000.0), None, Some(&m), &[], 0).market_type
        };
        assert_eq!(at(MarketCategory::Legal), 15.0);
        assert_eq!(at(MarketCategory::Politics), 9.0);
        assert_eq!(at(MarketCategory::CryptoPrice), 0.0);
        // Unknown market falls back to "other".
        assert_eq!(
            run(&trade(0.5, 2_000.0), None, None, &[], 0).market_type,
            7.5
        );
    }

    #[test]
    fn cluster_bonus_needs_min_wallets() {
        let m = market(MarketCategory::Other, None);
        let at = |n: usize| run(&trade(0.5, 2_000.0), None, Some(&m), &[], n).cluster_bonus;
        assert_eq!(at(0), 0.0);
        assert_eq!(at(1), 0.0);
        assert_eq!(at(2), 10.0);
        assert_eq!(at(5), 10.0);
    }

    #[test]
    fn sells_score_zero() {
        let w = wallet(Some(1.0), Some(1));
        let mut t = trade(0.03, 50_000.0);
        t.side = Side::Sell;
        let s = run(&t, Some(&w), None, &[], 3);
        assert_eq!(s.total, 0.0);
        assert_eq!(s.wallet_freshness, 0.0);
    }

    #[test]
    fn custom_weights_apply() {
        let mut c = cfg();
        c.weights.price_asymmetry = 40.0;
        let t = trade(0.02, 2_000.0);
        let s = score(
            &ScoreInput {
                trade: &t,
                wallet: None,
                market: None,
                positions: &[],
                fresh_cluster_wallets: 0,
            },
            &c,
        );
        assert_eq!(s.price_asymmetry, 40.0);
    }

    #[test]
    fn details_contain_every_component() {
        let s = run(&trade(0.1, 2_000.0), None, None, &[], 0);
        for key in [
            "wallet_freshness",
            "price_asymmetry",
            "relative_size",
            "concentration",
            "market_type",
            "cluster_bonus",
        ] {
            assert!(s.details.get(key).is_some(), "missing {key}");
        }
    }

    fn multiplicative() -> ScoringConfig {
        ScoringConfig {
            mode: ScoreMode::Multiplicative,
            ..cfg()
        }
    }

    fn run_with(
        c: &ScoringConfig,
        t: &TradeFacts,
        w: Option<&WalletRecord>,
        m: Option<&MarketRecord>,
        p: &[PositionRecord],
        cluster: usize,
    ) -> ScoreBreakdown {
        score(
            &ScoreInput {
                trade: t,
                wallet: w,
                market: m,
                positions: p,
                fresh_cluster_wallets: cluster,
            },
            c,
        )
    }

    #[test]
    fn multiplicative_caps_sports_and_ignores_new_wallet_concentration() {
        let c = multiplicative();
        // The live false positive: new wallet, all-in, big share of a thin
        // sports sub-market, no price asymmetry.
        let w = wallet(Some(2.0), Some(1));
        let sports = market(MarketCategory::Sports, Some(20_000.0));
        let s = run_with(&c, &trade(0.6, 5_400.0), Some(&w), Some(&sports), &[], 0);
        assert_eq!(
            s.concentration, 0.0,
            "fresh wallet: no concentration points"
        );
        assert_eq!(s.market_type, 0.0);
        // (25 + 20) / 95 * 100 * 0.1
        assert!((s.total - 4.74).abs() < 0.02, "total {}", s.total);
        // Same trade in additive mode is an alert.
        assert!(run(&trade(0.6, 5_400.0), Some(&w), Some(&sports), &[], 0).total >= 60.0);
    }

    #[test]
    fn multiplicative_keeps_textbook_insider_at_maximum() {
        let c = multiplicative();
        let w = wallet(Some(1.0), Some(1));
        let m = market(MarketCategory::Personnel, Some(10_000.0));
        let s = run_with(&c, &trade(0.03, 5_000.0), Some(&w), Some(&m), &[], 2);
        // fresh 25 + asym 25 + size 20 + conc 0 + cluster 10 = 80 of 95.
        assert!((s.total - 84.21).abs() < 0.02, "total {}", s.total);
        let sum = s.wallet_freshness
            + s.price_asymmetry
            + s.relative_size
            + s.concentration
            + s.cluster_bonus;
        assert!((sum - s.total).abs() < 0.05, "components add up to total");
    }

    #[test]
    fn multiplicative_counts_concentration_for_established_wallets() {
        let c = multiplicative();
        let w = wallet(Some(400.0), Some(300));
        let m = market(MarketCategory::Legal, None);
        let s = run_with(&c, &trade(0.5, 5_000.0), Some(&w), Some(&m), &[], 0);
        // Only concentration: 15 / 95 * 100.
        assert!(
            (s.concentration - 15.79).abs() < 0.02,
            "{}",
            s.concentration
        );
        assert_eq!(s.details["mode"], "multiplicative");
    }

    #[test]
    fn is_fresh_requires_full_freshness() {
        let c = cfg();
        assert!(is_fresh(Some(&wallet(Some(2.0), None)), NOW, &c));
        assert!(!is_fresh(Some(&wallet(Some(20.0), Some(50))), NOW, &c));
        assert!(!is_fresh(None, NOW, &c));
    }
}
