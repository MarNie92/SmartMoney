#![forbid(unsafe_code)]

//! Backtest of the insider score against resolved markets (no lookahead).
//!
//! For each sampled resolved market all trades are fetched. Every candidate
//! BUY is scored with only the information available at its timestamp, then
//! compared with the market's outcome and with following it X minutes later.

pub mod bots;
pub mod calibration;
pub mod dataset;
pub mod engine;
pub mod fullhistory;
pub mod report;
pub mod smart;
pub mod statusquo;
pub mod weather;
pub mod weather_experts;
pub mod weather_load;
pub mod weather_v2;

use engine::{evaluate, Evaluated, Variant};
use report::GroupStats;
use serde::Serialize;
use smartmoney_core::config::{Config, ScoreMode};

/// The variants compared by default.
pub fn default_variants(cfg: &Config) -> Vec<Variant> {
    let min_usd = cfg.collector.minimum_trade_usd;
    vec![
        Variant {
            name: format!("A: aktuell (additiv, ab ${min_usd:.0})"),
            mode: ScoreMode::Additive,
            min_usd,
            min_payout: None,
            min_payout_price: 0.0,
        },
        Variant {
            name: format!("B: Marktart als Multiplikator (ab ${min_usd:.0})"),
            mode: ScoreMode::Multiplicative,
            min_usd,
            min_payout: None,
            min_payout_price: 0.0,
        },
        Variant {
            name: format!(
                "C: Multiplikator + Auszahlungsfilter (ab ${min_usd:.0} oder Auszahlung ab ${:.0} bei Kurs ab {:.0}¢)",
                cfg.backtest.variant_min_payout_usd,
                cfg.backtest.variant_min_payout_price * 100.0
            ),
            mode: ScoreMode::Multiplicative,
            min_usd,
            min_payout: Some(cfg.backtest.variant_min_payout_usd),
            min_payout_price: cfg.backtest.variant_min_payout_price,
        },
    ]
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantResult {
    pub name: String,
    pub by_score: Vec<GroupStats>,
    pub by_category: Vec<GroupStats>,
    /// The classic insider signature: fresh wallet buying a longshot.
    pub signature: Vec<GroupStats>,
}

pub fn run_variants(data: &dataset::Dataset, cfg: &Config) -> Vec<VariantResult> {
    run_given_variants(
        data,
        cfg,
        default_variants(cfg),
        cfg.scoring.alert_threshold,
    )
}

type RowFilter<'a> = Box<dyn Fn(&Evaluated) -> bool + 'a>;

/// Compare fresh wallets buying longshots with everything else.
pub fn signature_groups(rows: &[Evaluated], cfg: &Config) -> Vec<GroupStats> {
    let bt = &cfg.backtest;
    let fresh_days = cfg.scoring.fresh_full_age_days;
    let longshot = cfg.scoring.asymmetry_zero_price;
    let is_fresh = |r: &Evaluated| r.wallet_age_days.is_some_and(|d| d <= fresh_days);
    let groups: [(&str, RowFilter<'_>); 4] = [
        (
            "Frisch+Außenseiter",
            Box::new(|r| is_fresh(r) && r.price <= longshot),
        ),
        (
            "Frisch, Rest",
            Box::new(|r| is_fresh(r) && r.price > longshot),
        ),
        (
            "Alt+Außenseiter",
            Box::new(|r| !is_fresh(r) && r.price <= longshot),
        ),
        (
            "Alt, Rest",
            Box::new(|r| !is_fresh(r) && r.price > longshot),
        ),
    ];
    groups
        .iter()
        .map(|(label, pred)| {
            let members: Vec<&Evaluated> = rows.iter().filter(|r| pred(r)).collect();
            report::group_stats(label, &members, &bt.follow_delays_minutes, bt.stake_usd)
        })
        .collect()
}

/// Variants for the targeted insider test (niche categories, lower size
/// threshold).
pub fn insider_variants(min_usd: f64) -> Vec<Variant> {
    vec![
        Variant {
            name: format!("A: aktuell (additiv, ab ${min_usd:.0})"),
            mode: ScoreMode::Additive,
            min_usd,
            min_payout: None,
            min_payout_price: 0.0,
        },
        Variant {
            name: format!("B: Marktart als Multiplikator (ab ${min_usd:.0})"),
            mode: ScoreMode::Multiplicative,
            min_usd,
            min_payout: None,
            min_payout_price: 0.0,
        },
    ]
}

/// Evaluate variants; the category table covers scores >= `category_min_score`.
pub fn run_given_variants(
    data: &dataset::Dataset,
    cfg: &Config,
    variants: Vec<Variant>,
    category_min_score: f64,
) -> Vec<VariantResult> {
    let bt = &cfg.backtest;
    variants
        .into_iter()
        .map(|v| {
            let rows = evaluate(
                &data.cases,
                &data.wallets,
                &v,
                &cfg.scoring,
                bt,
                cfg.simulation.fee_bps,
            );
            VariantResult {
                by_score: report::by_score(
                    &rows,
                    &bt.score_buckets,
                    &bt.follow_delays_minutes,
                    bt.stake_usd,
                ),
                signature: signature_groups(&rows, cfg),
                by_category: report::by_category(
                    &rows,
                    category_min_score,
                    &bt.follow_delays_minutes,
                    bt.stake_usd,
                ),
                name: v.name,
            }
        })
        .collect()
}
