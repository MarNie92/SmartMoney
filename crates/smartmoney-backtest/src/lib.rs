#![forbid(unsafe_code)]

//! Backtest of the insider score against resolved markets (no lookahead).
//!
//! For each sampled resolved market all trades are fetched. Every candidate
//! BUY is scored with only the information available at its timestamp, then
//! compared with the market's outcome and with following it X minutes later.

pub mod dataset;
pub mod engine;
pub mod report;

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
        },
        Variant {
            name: format!("B: Marktart als Multiplikator (ab ${min_usd:.0})"),
            mode: ScoreMode::Multiplicative,
            min_usd,
            min_payout: None,
        },
        Variant {
            name: format!(
                "C: Multiplikator + Auszahlungsfilter (ab ${min_usd:.0} oder Auszahlung ab ${:.0})",
                cfg.backtest.variant_min_payout_usd
            ),
            mode: ScoreMode::Multiplicative,
            min_usd,
            min_payout: Some(cfg.backtest.variant_min_payout_usd),
        },
    ]
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantResult {
    pub name: String,
    pub by_score: Vec<GroupStats>,
    pub by_category: Vec<GroupStats>,
}

pub fn run_variants(data: &dataset::Dataset, cfg: &Config) -> Vec<VariantResult> {
    let bt = &cfg.backtest;
    default_variants(cfg)
        .into_iter()
        .map(|v| {
            let rows: Vec<Evaluated> = evaluate(
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
                by_category: report::by_category(
                    &rows,
                    cfg.scoring.alert_threshold,
                    &bt.follow_delays_minutes,
                    bt.stake_usd,
                ),
                name: v.name,
            }
        })
        .collect()
}
