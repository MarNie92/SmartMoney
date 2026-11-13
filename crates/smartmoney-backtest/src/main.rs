//! `smartmoney-backtest`: replay resolved markets.
//!
//! - `score` (default): compare score variants on a broad market sample.
//! - `insider`: targeted test on personnel / corporate / legal markets.
//! - `smart`: do wallets that beat the market keep doing so?

use anyhow::Context;
use clap::{Args as ClapArgs, Parser, Subcommand};
use futures::stream::{self, StreamExt};
use smartmoney_backtest::bots::{self, PnlBreakdown};
use smartmoney_backtest::calibration;
use smartmoney_backtest::dataset::{self, Dataset, LoadOptions};
use smartmoney_backtest::fullhistory::{self, FullHistoryOptions};
use smartmoney_backtest::nowcast;
use smartmoney_backtest::reaction;
use smartmoney_backtest::smart::{self, SmartConfig};
use smartmoney_backtest::statusquo;
use smartmoney_backtest::{anchor, anchor_load};
use smartmoney_backtest::{
    insider_variants, report, run_given_variants, run_variants, VariantResult,
};
use smartmoney_backtest::{weather, weather_experts, weather_load, weather_v2};
use smartmoney_collector::PolymarketClient;
use smartmoney_core::{Config, MarketCategory};
use smartmoney_store::Store;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

const DAY: i64 = 86_400;

#[derive(Debug, Parser)]
#[command(
    name = "smartmoney-backtest",
    about = "Replays resolved Polymarket markets and reports how signals performed"
)]
struct Cli {
    #[command(flatten)]
    common: Common,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, ClapArgs)]
struct Common {
    /// Requests per 10 seconds (overrides config).
    #[arg(long, global = true)]
    rps10: Option<u32>,
    /// Directory for cached API responses.
    #[arg(long, global = true, default_value = "backtest_cache")]
    cache_dir: PathBuf,
    /// Also write the results as JSON to this file.
    #[arg(long, global = true)]
    json: Option<PathBuf>,
    /// Do not store the run in the database.
    #[arg(long, global = true)]
    no_save: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compare score variants on a broad sample (default).
    Score {
        #[arg(long)]
        markets_per_niche_tag: Option<usize>,
        #[arg(long)]
        markets_per_control_tag: Option<usize>,
        #[arg(long)]
        lookback_days: Option<u64>,
    },
    /// Targeted insider test: personnel, corporate and legal markets only.
    Insider {
        #[arg(long, default_value_t = 150)]
        markets_per_tag: usize,
        #[arg(long, default_value_t = 365)]
        lookback_days: u64,
        /// Minimum trade size (USDC) for scored trades.
        #[arg(long, default_value_t = 500.0)]
        min_usd: f64,
    },
    /// Smart-money persistence: rank wallets in period 1, test in period 2.
    Smart {
        /// Markets per tag and 30-day window (balanced over time).
        #[arg(long, default_value_t = 15)]
        markets_per_tag_month: usize,
        /// Split dates in days before today, comma separated.
        #[arg(long, value_delimiter = ',', default_values_t = [270i64, 180, 90])]
        splits: Vec<i64>,
        /// Length of period 1 (markets ending before the split), in days.
        #[arg(long, default_value_t = 90)]
        p1_days: i64,
        /// Length of period 2 (trades after the split), in days.
        #[arg(long, default_value_t = 90)]
        p2_days: i64,
        /// Minimum trades in period 1 (default 20, with --full-history 50).
        #[arg(long)]
        p1_min_trades: Option<usize>,
        /// Minimum markets in period 1 (default 5, with --full-history 10).
        #[arg(long)]
        p1_min_markets: Option<usize>,
        /// Minimum trades in period 2 (default 10, with --full-history 30).
        #[arg(long)]
        p2_min_trades: Option<usize>,
        /// Copy test: follow period-2 trades of at least this size.
        #[arg(long, default_value_t = 100.0)]
        copy_min_usd: f64,
        /// Rate wallets on their complete trade history instead of the
        /// trades seen in the market sample.
        #[arg(long)]
        full_history: bool,
        /// With --full-history: number of most active sample wallets.
        #[arg(long, default_value_t = 1500)]
        candidates: usize,
        /// With --full-history: skip wallets with more pages of 1000 trades.
        #[arg(long, default_value_t = 10)]
        max_pages_per_wallet: usize,
    },
    /// Calibration: do outcomes traded at price p win p of the time?
    Calibration {
        /// Markets per tag and 30-day window.
        #[arg(long, default_value_t = 15)]
        markets_per_tag_month: usize,
        #[arg(long, default_value_t = BALANCED_LOOKBACK_DAYS)]
        lookback_days: i64,
    },
    /// Who makes money and how: PnL composition and behaviour of bots,
    /// active traders and leaderboard wallets.
    Bots {
        #[arg(long, default_value_t = 15)]
        markets_per_tag_month: usize,
        #[arg(long, default_value_t = BALANCED_LOOKBACK_DAYS)]
        lookback_days: i64,
        /// Most active sample wallets to classify (bot vs. active trader).
        #[arg(long, default_value_t = 1500)]
        candidates: usize,
        /// Wallets with more pages of 1000 trades in the range count as bots.
        #[arg(long, default_value_t = 10)]
        max_pages_per_wallet: usize,
        /// Leaderboard size (all-time, by PnL and by volume).
        #[arg(long, default_value_t = 200)]
        leaderboard: u32,
    },
    /// Status-quo strategy: buy NO in "will X happen by <date>" markets.
    StatusQuo {
        #[arg(long, default_value_t = 15)]
        markets_per_tag_month: usize,
        #[arg(long, default_value_t = BALANCED_LOOKBACK_DAYS)]
        lookback_days: i64,
        /// Also use the niche sample (personnel/corporate/legal, 365 days).
        #[arg(long, default_value_t = true)]
        with_niche: bool,
        /// Minimum size (USDC) of the trade that sets the entry price.
        #[arg(long, default_value_t = 100.0)]
        min_trade_usd: f64,
    },
    /// Weather model vs. market on daily highest/lowest temperature markets.
    Weather {
        /// Markets that ended within this many days.
        #[arg(long, default_value_t = 60)]
        days: i64,
        /// Days of resolved history used to learn forecast bias and spread.
        #[arg(long, default_value_t = 45)]
        history_days: i64,
        #[arg(long, default_value_t = 10)]
        min_history: usize,
        /// Only trades of at least this size set the market price.
        #[arg(long, default_value_t = 2.0)]
        min_price_usd: f64,
        /// Only prices at most this old at the decision time (minutes).
        #[arg(long)]
        max_price_age_minutes: Option<i64>,
        /// Also report the strategy separately for the first and second half
        /// of the period (by event date).
        #[arg(long)]
        halves: bool,
    },
    /// Weather experts: rank wallets in temperature markets, test
    /// persistence and copy returns; optionally profile one wallet.
    WeatherExperts {
        #[arg(long, default_value_t = 120)]
        days: i64,
        /// Profile these wallets over the whole period (comma separated).
        #[arg(long, value_delimiter = ',')]
        wallet: Vec<String>,
        #[arg(long, default_value_t = 100)]
        p1_min_trades: usize,
        #[arg(long, default_value_t = 20)]
        p1_min_events: usize,
        #[arg(long, default_value_t = 50)]
        p2_min_trades: usize,
        /// Copy only trades of at least this size.
        #[arg(long, default_value_t = 10.0)]
        copy_min_usd: f64,
        /// Robustness grid: 3 split dates x top 10/20/40/80 x slippage
        /// 0.5 / 1 / 2 %, copy after 1, 5 and 15 minutes.
        #[arg(long)]
        robust: bool,
    },
    /// Weather model v2 (multi-model, station-calibrated) with the
    /// pre-registered success criterion.
    WeatherV2 {
        #[arg(long, default_value_t = 120)]
        days: i64,
        #[arg(long, default_value_t = 60)]
        history_days: i64,
        #[arg(long, default_value_t = 20)]
        min_history: usize,
        /// Spread inflation (the error fit is in-sample).
        #[arg(long, default_value_t = 1.15)]
        sd_inflation: f64,
    },
    /// Fingerprint of the best weather traders: when, how early and which
    /// buckets they trade compared with everyone else.
    WeatherFingerprint {
        #[arg(long, default_value_t = 120)]
        days: i64,
        /// Size of the top group (by USD-weighted edge).
        #[arg(long, default_value_t = 40)]
        top: usize,
        /// Minimum trades for a wallet to be ranked.
        #[arg(long, default_value_t = 300)]
        min_trades: usize,
    },
    /// Nowcast backtest on the event day: observed extreme so far plus the
    /// forecast for the remaining hours (pre-registered criterion at 12:58).
    WeatherNowcast {
        #[arg(long, default_value_t = 120)]
        days: i64,
        #[arg(long, default_value_t = 60)]
        history_days: i64,
        #[arg(long, default_value_t = 20)]
        min_history: usize,
        #[arg(long, default_value_t = 1.15)]
        sd_inflation: f64,
        /// Only stations with this unit ("f" or "c"); exploratory filter.
        #[arg(long)]
        unit: Option<String>,
    },
    /// Diagnostic: does the observed daily extreme (IEM routine METARs) fall
    /// into the bucket that actually won?
    WeatherTruthCheck {
        #[arg(long, default_value_t = 120)]
        days: i64,
    },
    /// Phase-1 diagnostic: how long do buckets ruled out by a routine METAR
    /// keep trading at stale prices, and who takes them how fast?
    WeatherReaction {
        #[arg(long, default_value_t = 120)]
        days: i64,
    },
    /// Sharp-line anchor backtest: football moneyline vs de-vigged closing
    /// odds (pre-registered criterion, ROADMAP Phase 1g).
    Anchor {
        /// Minimum trade size for the pre-kickoff price (USD).
        #[arg(long, default_value_t = 10.0)]
        min_trade_usd: f64,
        /// Price window starts this many minutes before kickoff.
        #[arg(long, default_value_t = 60)]
        window_minutes: i64,
        /// Prices younger than this many minutes before kickoff are ignored.
        #[arg(long, default_value_t = 5)]
        cutoff_minutes: i64,
    },
}

fn print_load_stats(s: &dataset::LoadStats) {
    println!(
        "{} Märkte ausgewertet ({} ausgewählt, {} ohne Auflösung, {} zu viele Trades, {} Fehler), {} Trades, {} Wallets.",
        s.markets_used,
        s.markets_selected,
        s.markets_unresolved,
        s.markets_too_many_trades,
        s.markets_failed,
        s.trades,
        s.wallets
    );
    if s.payouts_from_gamma > 0 || s.payout_source_mismatches > 0 {
        println!(
            "Auflösung: {} Märkte über Gamma-Endkurse (keine payouts in /v2/resolutions), {} Abweichungen zwischen beiden Quellen.",
            s.payouts_from_gamma, s.payout_source_mismatches
        );
    }
}

fn print_variants(cfg: &Config, results: &[VariantResult], category_title: &str) {
    println!(
        "Treffer = Anteil gewonnener Käufe. Ø Kurs = vom Markt eingepreiste Gewinnchance. \
         Edge = Treffer - Ø Kurs (> 0: besser als der Markt). ROI +Xm = Rendite beim Nachziehen X Minuten später \
         zum nächsten echten Trade (+{:.0} bps Slippage), in Klammern die Anzahl möglicher Einstiege.",
        cfg.backtest.slippage_bps
    );
    for r in results {
        print!(
            "{}",
            report::table(&format!("== {} ==", r.name), &r.by_score)
        );
        print!(
            "{}",
            report::table(
                &format!(
                    "   Insider-Signatur (frisch = bis {:.0} Tage, Außenseiter = Kurs bis {:.0}¢)",
                    cfg.scoring.fresh_full_age_days,
                    cfg.scoring.asymmetry_zero_price * 100.0
                ),
                &r.signature
            )
        );
        print!("{}", report::table(category_title, &r.by_category));
    }
    println!(
        "\nHinweise: Wallet-Alter aus join_date; die Anzahl gehandelter Märkte fließt nicht ein (nur heutiger Wert verfügbar). \
         Konzentration aus den Käufen der letzten {} Tage angenähert. Märkte mit mehr als {} Trades werden übersprungen.",
        cfg.backtest.wallet_history_days, cfg.backtest.max_trades_per_market
    );
}

async fn load(
    client: &PolymarketClient,
    cfg: &Config,
    opts: &LoadOptions,
    now: i64,
) -> anyhow::Result<Dataset> {
    Ok(dataset::load(client, &cfg.backtest, opts, now).await?)
}

/// Balanced sample: every niche and control tag fills its quota in each
/// 30-day end-date window over `lookback_days` (an even sample over time
/// instead of only the most recently ended markets).
fn balanced_options(
    cfg: &mut Config,
    now: i64,
    lookback_days: i64,
    per_tag_month: usize,
) -> LoadOptions {
    cfg.backtest.markets_per_niche_tag = per_tag_month;
    cfg.backtest.markets_per_control_tag = per_tag_month;
    cfg.backtest.lookback_days = lookback_days as u64;
    let windows: Vec<(i64, i64)> = (0..(lookback_days + 29) / 30)
        .map(|k| (now - (k + 1) * 30 * DAY, now - k * 30 * DAY))
        .collect();
    LoadOptions {
        categories: None,
        wallets: false,
        max_event_pages: 40,
        windows: Some(windows),
    }
}

/// Lookback of the default smart-money run (270 + 90 + 3 days); reusing it
/// lets other analyses hit the same cache.
const BALANCED_LOOKBACK_DAYS: i64 = 363;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let common = cli.common;

    let mut cfg = Config::load().context("loading configuration")?;
    if let Some(r) = common.rps10 {
        cfg.backtest.requests_per_10s = r;
    }
    let client = PolymarketClient::new(&cfg.collector)?
        .with_rate(cfg.backtest.requests_per_10s)
        .with_cache(&common.cache_dir)
        .with_context(|| format!("creating cache dir {}", common.cache_dir.display()))?;
    let now = chrono::Utc::now().timestamp();

    let command = cli.command.unwrap_or(Command::Score {
        markets_per_niche_tag: None,
        markets_per_control_tag: None,
        lookback_days: None,
    });

    let (kind, output) = match command {
        Command::Score {
            markets_per_niche_tag,
            markets_per_control_tag,
            lookback_days,
        } => {
            if let Some(n) = markets_per_niche_tag {
                cfg.backtest.markets_per_niche_tag = n;
            }
            if let Some(n) = markets_per_control_tag {
                cfg.backtest.markets_per_control_tag = n;
            }
            if let Some(d) = lookback_days {
                cfg.backtest.lookback_days = d;
            }
            let data = load(&client, &cfg, &LoadOptions::default(), now).await?;
            println!("SmartMoney Backtest (Score-Varianten)");
            print_load_stats(&data.stats);
            let results = run_variants(&data, &cfg);
            print_variants(
                &cfg,
                &results,
                &format!(
                    "   nach Kategorie (Score ab {:.0})",
                    cfg.scoring.alert_threshold
                ),
            );
            (
                "score",
                serde_json::json!({ "stats": data.stats, "results": results }),
            )
        }
        Command::Insider {
            markets_per_tag,
            lookback_days,
            min_usd,
        } => {
            cfg.backtest.markets_per_niche_tag = markets_per_tag;
            cfg.backtest.control_tags.clear();
            cfg.backtest.lookback_days = lookback_days;
            cfg.backtest.min_candidate_usd = cfg.backtest.min_candidate_usd.min(min_usd);
            let opts = LoadOptions {
                categories: Some(HashSet::from([
                    MarketCategory::Personnel,
                    MarketCategory::Corporate,
                    MarketCategory::Legal,
                ])),
                wallets: true,
                max_event_pages: 80,
                windows: None,
            };
            let data = load(&client, &cfg, &opts, now).await?;
            println!(
                "SmartMoney Backtest (gezielter Insider-Test: Personalien, Firmen, Gerichte, {lookback_days} Tage, ab ${min_usd:.0})"
            );
            print_load_stats(&data.stats);
            let results = run_given_variants(&data, &cfg, insider_variants(min_usd), 0.0);
            print_variants(&cfg, &results, "   nach Kategorie (alle Scores)");
            (
                "insider",
                serde_json::json!({ "stats": data.stats, "results": results }),
            )
        }
        Command::Smart {
            markets_per_tag_month,
            splits,
            p1_days,
            p2_days,
            p1_min_trades,
            p1_min_markets,
            p2_min_trades,
            copy_min_usd,
            full_history,
            candidates,
            max_pages_per_wallet,
        } => {
            let (d1, d2, d3) = if full_history {
                (50, 10, 30)
            } else {
                (20, 5, 10)
            };
            let p1_min_trades = p1_min_trades.unwrap_or(d1);
            let p1_min_markets = p1_min_markets.unwrap_or(d2);
            let p2_min_trades = p2_min_trades.unwrap_or(d3);
            let margin_days = 3;
            let oldest_split = splits.iter().copied().max().unwrap_or(180);
            let lookback = oldest_split + p1_days + margin_days;
            let opts = balanced_options(&mut cfg, now, lookback, markets_per_tag_month);
            let data = load(&client, &cfg, &opts, now).await?;
            let load_stats = data.stats.clone();
            let (cases, full_stats) = if full_history {
                let fh = FullHistoryOptions {
                    candidates,
                    range_start: now - lookback * DAY,
                    now,
                    max_pages_per_wallet,
                };
                let (cases, stats) = fullhistory::load(&client, data.cases, &fh).await;
                (cases, Some(stats))
            } else {
                (data.cases, None)
            };
            println!(
                "SmartMoney Backtest (Smart-Money-Persistenz{}, {} Stichtage, Perioden je {p1_days}/{p2_days} Tage)",
                if full_history { " mit kompletter Wallet-Historie" } else { "" },
                splits.len()
            );
            print_load_stats(&load_stats);
            if let Some(f) = &full_stats {
                println!(
                    "Wallet-Historien: {} von {} Wallets geladen ({} zu aktiv/Bots, {} Fehler), {} Trades. \
                     Märkte: {} aus der Stichprobe, {} zusätzlich über Gamma aufgelöst, {} noch offen/ohne Endkurs.",
                    f.wallets_loaded,
                    f.candidates,
                    f.wallets_too_active,
                    f.wallets_failed,
                    f.wallet_trades,
                    f.markets_from_sample,
                    f.markets_resolved_extra,
                    f.markets_unresolved
                );
                println!(
                    "Wallets bewertet ab {p1_min_trades} Trades in {p1_min_markets} Märkten (P1) und {p2_min_trades} Trades (P2). \
                     Der Kopie-Test nutzt nur Märkte der Stichprobe (dort sind alle Trades für den Nachzieh-Kurs bekannt)."
                );
            }
            println!(
                "Edge = Vorsprung pro Trade gegenüber dem Endergebnis (Kauf: Auszahlung - Kurs, Verkauf: Kurs - Auszahlung), \
                 je Wallet nach Einsatz gewichtet. Gruppen nach Edge in Periode 1 sortiert (nur Märkte, die mindestens \
                 {margin_days} Tage vor dem Stichtag endeten). Kopie +Xm = Ø Rendite beim Nachziehen aller Trades ab \
                 ${copy_min_usd:.0} der Gruppe in Periode 2; Verkäufe als Kauf der Gegenseite (Einstiege/Märkte)."
            );
            let mut reports = Vec::new();
            for split_days in &splits {
                let split_ts = now - split_days * DAY;
                let smart_cfg = SmartConfig {
                    split_ts,
                    p1_start_ts: split_ts - p1_days * DAY,
                    p2_end_ts: split_ts + p2_days * DAY,
                    resolve_margin_days: margin_days,
                    p1_min_trades,
                    p1_min_markets,
                    p2_min_trades,
                    groups: 5,
                    copy_min_usd,
                    follow_delays_minutes: cfg.backtest.follow_delays_minutes.clone(),
                    slippage_bps: cfg.backtest.slippage_bps,
                    fee_bps: cfg.simulation.fee_bps,
                };
                let r = smart::evaluate(&cases, &smart_cfg);
                println!(
                    "\n== Stichtag vor {split_days} Tagen: P1 {} Märkte, P2 {} Märkte, {} Wallets, Rangkorrelation {} ==",
                    r.p1_markets,
                    r.p2_markets,
                    r.eligible_wallets,
                    r.spearman.map_or("–".into(), |s| format!("{s:+.2}"))
                );
                print!("{}", report::smart_table(&r));
                reports.push(serde_json::json!({ "split_days_ago": split_days, "report": r }));
            }
            (
                "smart",
                serde_json::json!({ "stats": load_stats, "full_history": full_stats, "splits": reports }),
            )
        }
        Command::Calibration {
            markets_per_tag_month,
            lookback_days,
        } => {
            let opts = balanced_options(&mut cfg, now, lookback_days, markets_per_tag_month);
            let data = load(&client, &cfg, &opts, now).await?;
            println!(
                "SmartMoney Kalibrierung: Gewinnt ein Outcome, das zu Kurs p gehandelt wird, in p der Fälle?"
            );
            print_load_stats(&data.stats);
            println!(
                "Jeder Trade ist eine Beobachtung (Kurs des gehandelten Outcomes, Endergebnis). Edge = Treffer - Ø Kurs: \
                 > 0 heißt unterbewertet (Kaufen war im Schnitt profitabel), < 0 überbewertet. 95%-Bereich nach Märkten \
                 gruppiert. Kauf-Rendite = Treffer / Ø Kurs - 1, vor Gebühren und Slippage.\n"
            );
            let buckets = calibration::by_price(
                &data.cases,
                &[
                    0.02, 0.05, 0.10, 0.20, 0.35, 0.50, 0.65, 0.80, 0.90, 0.95, 0.98,
                ],
            );
            print!("{}", report::calibration_table(&buckets));
            let categories = calibration::by_category(&data.cases);
            print!("{}", report::zone_table("nach Kategorie", &categories));
            let horizons = calibration::by_horizon(&data.cases);
            print!(
                "{}",
                report::zone_table("nach Zeit bis Marktende", &horizons)
            );
            (
                "calibration",
                serde_json::json!({
                    "stats": data.stats,
                    "by_price": buckets,
                    "by_category": categories,
                    "by_horizon": horizons,
                }),
            )
        }
        Command::Bots {
            markets_per_tag_month,
            lookback_days,
            candidates,
            max_pages_per_wallet,
            leaderboard,
        } => {
            let opts = balanced_options(&mut cfg, now, lookback_days, markets_per_tag_month);
            let data = load(&client, &cfg, &opts, now).await?;

            // 1. Classify the most active sample wallets.
            let fh = FullHistoryOptions {
                candidates,
                range_start: now - lookback_days * DAY,
                now,
                max_pages_per_wallet,
            };
            let picked = fullhistory::pick_candidates(&data.cases, candidates);
            tracing::info!(wallets = picked.len(), "classifying wallets");
            let classified: Vec<(String, Result<bool, _>)> = stream::iter(picked)
                .map(|w| {
                    let client = &client;
                    let fh = &fh;
                    async move {
                        let r = fullhistory::is_too_active(client, &w, fh).await;
                        (w, r)
                    }
                })
                .buffer_unordered(8)
                .collect()
                .await;
            let mut bots = Vec::new();
            let mut traders = Vec::new();
            for (w, r) in classified {
                match r {
                    Ok(true) => bots.push(w),
                    Ok(false) => traders.push(w),
                    Err(e) => tracing::warn!(wallet = %w, error = %e, "classification skipped"),
                }
            }
            bots.sort();
            traders.sort();

            // 2. Leaderboards (all-time).
            let top_pnl = client.leaderboard("all", "PNL", leaderboard).await?;
            let top_volume = client.leaderboard("all", "VOLUME", leaderboard).await?;

            // 3. PnL composition for every wallet in any group.
            let all: HashSet<String> = bots
                .iter()
                .chain(&traders)
                .chain(&top_pnl)
                .chain(&top_volume)
                .cloned()
                .collect();
            tracing::info!(wallets = all.len(), "loading PnL composition");
            let pnl: HashMap<String, PnlBreakdown> = stream::iter(all.iter().cloned())
                .map(|w| {
                    let client = &client;
                    async move {
                        let r = client.user_stats(&w).await;
                        (w, r)
                    }
                })
                .buffer_unordered(8)
                .filter_map(|(w, r)| async move {
                    match r {
                        Ok(stats) => PnlBreakdown::from_user_stats(&stats.raw).map(|p| (w, p)),
                        Err(e) => {
                            tracing::warn!(wallet = %w, error = %e, "user stats skipped");
                            None
                        }
                    }
                })
                .collect()
                .await;
            let behavior = bots::behaviors(&data.cases, &all);

            let groups = vec![
                bots::profile("Bots (> 10.000 Trades/Jahr)", &bots, &pnl, &behavior),
                bots::profile("Aktive Trader", &traders, &pnl, &behavior),
                bots::profile(
                    &format!("Rangliste Gewinn Top {leaderboard}"),
                    &top_pnl,
                    &pnl,
                    &behavior,
                ),
                bots::profile(
                    &format!("Rangliste Volumen Top {leaderboard}"),
                    &top_volume,
                    &pnl,
                    &behavior,
                ),
            ];
            println!("SmartMoney Bot-Profil: Wer verdient auf Polymarket, und womit?");
            print_load_stats(&data.stats);
            println!(
                "Gruppen: die {candidates} aktivsten Wallets der Stichprobe, getrennt in Bots (mehr als {} Trades in {lookback_days} Tagen) \
                 und aktive Trader, dazu die Allzeit-Ranglisten nach Gewinn und Volumen. Beträge aus /v2/user-stats all_time_pnl \
                 (gesamte Laufzeit, USDC): Gesamt = economic_pnl, Handel real. = realisierter Markt- und Kombi-Gewinn, \
                 Rebates = Maker + Taker, Prämien = Belohnungen, Zinsen, Empfehlungen. Volumen = volume_usdc (laut Doku \
                 maker-attributiert), Marge = Gesamt / Volumen. Fehlende Werte zählen nicht als null.\n",
                max_pages_per_wallet * 1000
            );
            print!("{}", report::bots_table(&groups));
            (
                "bots",
                serde_json::json!({ "stats": data.stats, "groups": groups }),
            )
        }
        Command::StatusQuo {
            markets_per_tag_month,
            lookback_days,
            with_niche,
            min_trade_usd,
        } => {
            let opts = balanced_options(&mut cfg, now, lookback_days, markets_per_tag_month);
            let mut data = load(&client, &cfg, &opts, now).await?;
            if with_niche {
                // Same selection as the `insider` command (cached), without
                // wallet facts.
                let mut niche_cfg = cfg.clone();
                niche_cfg.backtest.markets_per_niche_tag = 150;
                niche_cfg.backtest.control_tags.clear();
                niche_cfg.backtest.lookback_days = 365;
                let niche_opts = LoadOptions {
                    categories: Some(HashSet::from([
                        MarketCategory::Personnel,
                        MarketCategory::Corporate,
                        MarketCategory::Legal,
                    ])),
                    wallets: false,
                    max_event_pages: 80,
                    windows: None,
                };
                let niche = load(&client, &niche_cfg, &niche_opts, now).await?;
                let known: HashSet<String> = data
                    .cases
                    .iter()
                    .map(|c| c.market.condition_id.clone())
                    .collect();
                data.cases.extend(
                    niche
                        .cases
                        .into_iter()
                        .filter(|c| !known.contains(&c.market.condition_id)),
                );
            }
            let rules: Vec<statusquo::SqRule> =
                [(0.80, 0.90), (0.90, 0.95), (0.95, 0.98), (0.98, 0.995)]
                    .iter()
                    .flat_map(|band| {
                        [1i64, 7, 30].map(|min_days| statusquo::SqRule {
                            band: *band,
                            min_days,
                            min_trade_usd,
                        })
                    })
                    .collect();
            let stake = cfg.backtest.stake_usd;
            let (sq, control) =
                statusquo::evaluate(&data.cases, &rules, cfg.backtest.slippage_bps, stake);
            let sq_markets = data
                .cases
                .iter()
                .filter(|c| statusquo::is_status_quo_market(&c.market))
                .count();
            println!("SmartMoney Strategie-Test: Status quo (\"es passiert nichts\")");
            println!(
                "{} Märkte geladen, davon {sq_markets} Status-quo-Märkte (\"Will X ... by/before/in <Datum>?\", ohne Sport, Krypto, Wetter).",
                data.cases.len()
            );
            println!(
                "Regel: pro Markt einmal NEIN kaufen, beim ersten Trade ab ${min_trade_usd:.0} mit NEIN-Kurs im Band und mindestens N Tagen bis Marktende; \
                 halten bis zur Auflösung. Kosten: +{:.0} bps Slippage und Polymarket-Taker-Gebühr (Satz x p x (1-p), \
                 4-7 % je Kategorie). Einsatz ${stake:.0} pro Trade. 95%-Bereich nach Events gruppiert.",
                cfg.backtest.slippage_bps
            );
            print!(
                "{}",
                report::status_quo_table("== Status-quo-Märkte ==", &sq)
            );
            print!(
                "{}",
                report::status_quo_table(
                    "== Vergleich: alle anderen Ja/Nein-Märkte, gleiche Regel ==",
                    &control
                )
            );
            (
                "status_quo",
                serde_json::json!({ "markets": data.cases.len(), "status_quo_markets": sq_markets, "status_quo": sq, "control": control }),
            )
        }
        Command::Weather {
            days,
            history_days,
            min_history,
            min_price_usd,
            max_price_age_minutes,
            halves,
        } => {
            let data = weather_load::load(&client, days, history_days, now).await?;
            let s = &data.stats;
            println!("SmartMoney Wetter-Backtest: Wettermodell gegen Marktpreis (Höchst-/Tiefsttemperatur)");
            println!(
                "{} Events geladen ({} gesehen, {} nicht auswertbar), {} Stationen ({} ohne Vorhersage), {} Trades.",
                s.events_used, s.events_seen, s.events_skipped, s.stations, s.stations_without_forecast, s.trades
            );
            println!(
                "Modell: Open-Meteo-Vorhersage (best match), 24 h bzw. 48 h vor der jeweiligen Stunde erstellt, plus Bias und Streuung \
                 je Station aus den letzten {history_days} bereits aufgelösten Tagen (mind. {min_history}). Entscheidung: Vorlauf 1 Tag um 0 Uhr \
                 Ortszeit am Tag selbst, Vorlauf 2 Tage um 6 Uhr am Vortag. Marktpreis = letzter Trade ab ${min_price_usd:.0} davor.\n\
                 Brier = mittlerer quadratischer Fehler der Wahrscheinlichkeiten (kleiner ist besser). Vorteil > 0: Modell genauer als Markt.\n\
                 Wetten: JA wenn Modell um die Schwelle über dem Markt liegt, NEIN wenn darunter. Taker = Kurs + {:.0} bps + Gebühr \
                 (0,05 x p x (1-p)); *Maker = Ausführung zum Kurs ohne Gebühr (optimistisch). 95%-Bereich nach Events gruppiert.",
                cfg.backtest.slippage_bps
            );
            let mut leads = Vec::new();
            for lead in [2u8, 1] {
                let mcfg = weather::ModelConfig {
                    lead,
                    history_days,
                    min_history,
                    sd_floor_f: 1.0,
                    sd_floor_c: 0.6,
                    min_price_usd,
                    max_price_age: max_price_age_minutes.map(|m| m * 60),
                };
                let obs = weather::observations(&data.events, &data.trades, &data.forecasts, &mcfg);
                let mut scores = Vec::new();
                if let Some(c) = weather::compare(&obs) {
                    scores.push(("Alle".to_owned(), c));
                }
                for (label, kind) in [
                    ("Höchsttemperatur", weather::Kind::High),
                    ("Tiefsttemperatur", weather::Kind::Low),
                ] {
                    let sub: Vec<weather::Observation> =
                        obs.iter().filter(|o| o.kind == kind).cloned().collect();
                    if let Some(c) = weather::compare(&sub) {
                        scores.push((label.to_owned(), c));
                    }
                }
                let strategies: Vec<weather::StrategyResult> = [0.03, 0.05, 0.10, 0.15, 0.20]
                    .iter()
                    .filter_map(|t| {
                        weather::strategy(&obs, *t, 0.05, cfg.backtest.slippage_bps, 10.0)
                    })
                    .collect();
                println!(
                    "\n== Vorlauf {lead} Tag(e): {} Spannen mit Marktpreis ==",
                    obs.len()
                );
                print!("{}", report::weather_score_table(&scores));
                println!("\nKalibrierung (Gewinnquote je Wahrscheinlichkeitsbereich):");
                print!("{}", report::weather_calibration_table(&obs));
                println!("\nStrategie (Einsatz $10 pro Wette):");
                print!("{}", report::weather_strategy_table(&strategies));
                if halves && !obs.is_empty() {
                    let mut dates: Vec<_> = obs.iter().map(|o| o.date).collect();
                    dates.sort();
                    let mid = dates[dates.len() / 2];
                    for (label, first) in [("erste Hälfte", true), ("zweite Hälfte", false)] {
                        let part: Vec<weather::Observation> = obs
                            .iter()
                            .filter(|o| (o.date < mid) == first)
                            .cloned()
                            .collect();
                        let rows: Vec<weather::StrategyResult> = [0.10, 0.15, 0.20]
                            .iter()
                            .filter_map(|t| {
                                weather::strategy(&part, *t, 0.05, cfg.backtest.slippage_bps, 10.0)
                            })
                            .collect();
                        println!(
                            "\nStrategie, {label} ({} bis {}):",
                            part.iter()
                                .map(|o| o.date)
                                .min()
                                .map_or("–".into(), |d| d.to_string()),
                            part.iter()
                                .map(|o| o.date)
                                .max()
                                .map_or("–".into(), |d| d.to_string())
                        );
                        print!("{}", report::weather_strategy_table(&rows));
                    }
                }
                leads.push(serde_json::json!({ "lead": lead, "observations": obs.len(), "scores": scores, "strategies": strategies }));
            }
            (
                "weather",
                serde_json::json!({ "stats": data.stats, "leads": leads }),
            )
        }
        Command::WeatherExperts {
            days,
            wallet,
            p1_min_trades,
            p1_min_events,
            p2_min_trades,
            copy_min_usd,
            robust,
        } => {
            let data = weather_load::load(&client, days, 45, now).await?;
            let mut dates: Vec<_> = data.events.iter().map(|e| e.date).collect();
            dates.sort();
            let split_date = dates.get(dates.len() / 2).copied().unwrap_or_default();
            let ecfg = weather_experts::ExpertConfig {
                split_date,
                p1_min_trades,
                p1_min_events,
                p2_min_trades,
                groups: 5,
                top_n: 20,
                copy_min_usd,
                delays_minutes: vec![0, 1, 5, 15, 60],
                slippage_bps: cfg.backtest.slippage_bps,
                fee_rate: 0.05,
            };
            println!("SmartMoney Wetter-Experten: Lassen sich gute Wetter-Trader im Voraus erkennen und kopieren?");
            println!(
                "{} Temperatur-Events, {} Wallets, {} Trades. Periode 1: Events vor {split_date}, Periode 2: ab {split_date}.",
                data.events.len(),
                data.wallets.len(),
                data.stats.trades
            );
            println!(
                "Edge = Vorsprung pro Trade gegenüber dem Ergebnis (Prozentpunkte, nach Einsatz gewichtet). Bewertet werden Wallets \
                 mit mind. {p1_min_trades} Trades in {p1_min_events} Events (P1) und {p2_min_trades} Trades (P2). Kopie +X min = Ø Rendite, \
                 wenn man jedem Trade ab ${copy_min_usd:.0} der Gruppe in P2 nach X Minuten zum nächsten echten Kurs derselben Spanne \
                 folgt (+{:.0} bps Slippage, Gebühr 0,05 x p x (1-p)); in Klammern 95%-Bereich nach Events.\n",
                cfg.backtest.slippage_bps
            );
            for w in &wallet {
                match weather_experts::profile_wallet(
                    &data.events,
                    &data.trades,
                    &data.wallets,
                    w,
                    &ecfg,
                ) {
                    Some(p) => print!("{}", report::wallet_profile_table(&p)),
                    None => println!("Wallet {w}: keine Trades in den geladenen Wetter-Märkten."),
                }
            }
            if robust {
                // Split dates at 35 %, 50 % and 65 % of the event dates.
                let splits: Vec<_> = [0.35, 0.50, 0.65]
                    .iter()
                    .filter_map(|q| dates.get((dates.len() as f64 * q) as usize).copied())
                    .collect();
                let base = weather_experts::ExpertConfig {
                    delays_minutes: vec![1, 5, 15],
                    ..ecfg.clone()
                };
                let rows = weather_experts::robustness(
                    &data.events,
                    &data.trades,
                    &base,
                    &splits,
                    &[10, 20, 40, 80],
                    &[50.0, 100.0, 200.0],
                );
                println!(
                    "\nRobustheit: Top-N kopieren über {} Stichtage x Top 10/20/40/80 x Slippage 0,5/1/2 % (Gebühr immer enthalten).",
                    splits.len()
                );
                print!("{}", report::robustness_table(&rows));
                return finish(
                    &common,
                    &cfg,
                    now,
                    "weather_experts_robust",
                    serde_json::json!({ "rows": rows }),
                )
                .await;
            }
            let r = weather_experts::evaluate(&data.events, &data.trades, &ecfg);
            println!(
                "\n{} Wallets bewertet, Rangkorrelation Edge P1 zu P2: {}",
                r.eligible,
                r.spearman.map_or("–".into(), |s| format!("{s:+.2}"))
            );
            print!("{}", report::experts_table(&r));
            (
                "weather_experts",
                serde_json::json!({ "split_date": split_date, "report": r }),
            )
        }
        Command::WeatherV2 {
            days,
            history_days,
            min_history,
            sd_inflation,
        } => {
            let data = weather_load::load(&client, days, history_days, now).await?;
            let stations =
                weather_load::station_data_v2(&client, &data.events, history_days).await?;
            let with_obs = stations.values().filter(|s| !s.obs.is_empty()).count();
            println!("SmartMoney Wetter-Modell v2: 7 Wettermodelle, kalibriert auf Stationsmessungen (METAR)");
            println!(
                "{} Events, {} Stationen ({} mit Messwerten), {} Trades. Lernfenster {history_days} Tage (mind. {min_history}), \
                 Streuung x {sd_inflation}. Marktpreise höchstens 60 min alt.",
                data.events.len(),
                stations.len(),
                with_obs,
                data.stats.trades
            );
            let mut leads = Vec::new();
            for lead in [2u8, 1] {
                let vcfg = weather_v2::V2Config {
                    lead,
                    history_days,
                    min_history,
                    sd_inflation,
                    sd_floor_f: 1.0,
                    sd_floor_c: 0.6,
                    min_price_usd: 2.0,
                    max_price_age: Some(60 * 60),
                };
                let obs = weather_v2::observations(&data.events, &data.trades, &stations, &vcfg);
                let crit = weather_v2::criterion(
                    &obs,
                    true,
                    &[0.03, 0.05, 0.10, 0.15, 0.20],
                    200,
                    0.05,
                    cfg.backtest.slippage_bps,
                );
                println!(
                    "\n== Vorlauf {lead} Tag(e): {} Spannen mit frischem Marktpreis ==",
                    obs.len()
                );
                println!("Kalibrierung (Gewinnquote je Wahrscheinlichkeitsbereich):");
                print!("{}", report::weather_calibration_table(&obs));
                println!();
                print!("{}", report::criterion_table(&crit));
                leads.push(serde_json::json!({ "lead": lead, "observations": obs.len(), "criterion": crit }));
            }
            (
                "weather_v2",
                serde_json::json!({ "stats": data.stats, "leads": leads }),
            )
        }
        Command::WeatherFingerprint {
            days,
            top,
            min_trades,
        } => {
            use smartmoney_backtest::weather_fingerprint as fp;
            let data = weather_load::load(&client, days, 60, now).await?;
            let stations = weather_load::station_data_v2(&client, &data.events, 60).await?;
            let offsets: HashMap<String, i64> = stations
                .iter()
                .map(|(k, v)| (k.clone(), v.utc_offset))
                .collect();
            let vcfg = weather_v2::V2Config {
                lead: 1,
                history_days: 60,
                min_history: 20,
                sd_inflation: 1.15,
                sd_floor_f: 1.0,
                sd_floor_c: 0.6,
                min_price_usd: 0.0,
                max_price_age: None,
            };
            let models: HashMap<usize, (f64, f64)> = data
                .events
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let st = stations.get(&e.station)?;
                    Some((i, weather_v2::predict(st, e.kind, e.unit, e.date, &vcfg)?))
                })
                .collect();
            let ctx = fp::contexts(&data.events, &data.trades, &offsets, &models);
            let ranked = fp::top_wallets(&ctx, min_trades, top);
            let set: HashSet<u32> = ranked.iter().map(|r| r.0).collect();
            let top_trades = ctx.iter().filter(|c| set.contains(&c.wallet)).count();
            println!("SmartMoney Fingerabdruck der besten Wetter-Trader");
            println!(
                "{} Trades in {} Events. Top {} Wallets (mind. {min_trades} Trades, nach Edge über den ganzen Zeitraum): {} Trades, \
                 Edge {} bis {}. Faktor = Anteil Top / Anteil Rest (über 1 = Top handelt dort überproportional; '<' markiert >= 1,5).\n\
                 Hinweis: rein beschreibend über den ganzen Zeitraum, kein Handelstest.",
                ctx.len(),
                data.events.len(),
                ranked.len(),
                top_trades,
                ranked.last().map_or("–".into(), |r| format!("{:+.1}%", r.1 * 100.0)),
                ranked.first().map_or("–".into(), |r| format!("{:+.1}%", r.1 * 100.0)),
            );
            let hours: Vec<String> = (0..24).map(|h| format!("{h:02}:00")).collect();
            let slots: Vec<String> = (0..12)
                .map(|s| format!(":{:02}–:{:02}", s * 5, s * 5 + 4))
                .collect();
            let labels =
                |bins: &[(&str, f64, f64)]| bins.iter().map(|b| b.0.to_owned()).collect::<Vec<_>>();
            let day_labels: Vec<String> = fp::DAY_BINS.iter().map(|b| b.0.to_owned()).collect();
            let tables = [
                (
                    "Uhrzeit UTC (feste Modell-Veröffentlichungen wären hier sichtbar)",
                    fp::distribution(&ctx, &set, &hours, fp::utc_hour),
                ),
                (
                    "Ortszeit der Station",
                    fp::distribution(&ctx, &set, &hours, fp::local_hour),
                ),
                (
                    "Minute in der Stunde (US-METAR-Messungen erscheinen um :51–:53)",
                    fp::distribution(&ctx, &set, &slots, fp::minute_slot),
                ),
                (
                    "Zeitpunkt relativ zum Markttag (Ortszeit)",
                    fp::distribution(&ctx, &set, &day_labels, fp::day_bin),
                ),
                (
                    "Bezahlter Kurs (gekaufte Seite)",
                    fp::distribution(&ctx, &set, &labels(&fp::PRICE_BINS), fp::price_bin),
                ),
                (
                    "Gekaufte Spanne relativ zur Modellvorhersage (nur JA-Käufe)",
                    fp::distribution(&ctx, &set, &labels(&fp::Z_BINS), fp::z_bin_long),
                ),
            ];
            let mut json_tables = Vec::new();
            for (title, rows) in &tables {
                print!("{}", report::fingerprint_table(title, rows));
                json_tables.push(serde_json::json!({ "title": title, "rows": rows }));
            }
            let wallets: Vec<serde_json::Value> = ranked
                .iter()
                .map(|(w, e, n)| serde_json::json!({ "wallet": data.wallets[*w as usize], "edge": e, "trades": n }))
                .collect();
            (
                "weather_fingerprint",
                serde_json::json!({ "top_wallets": wallets, "tables": json_tables }),
            )
        }
        Command::WeatherNowcast {
            days,
            history_days,
            min_history,
            sd_inflation,
            unit,
        } => {
            let mut data = weather_load::load(&client, days, history_days, now).await?;
            let stations =
                weather_load::station_data_v2(&client, &data.events, history_days).await?;
            if let Some(u) = unit.as_deref() {
                let want = if u.eq_ignore_ascii_case("f") {
                    weather::Unit::F
                } else {
                    weather::Unit::C
                };
                data.events.retain(|e| e.unit == want);
                println!(
                    "ERKUNDUNG: nur Stationen mit Einheit {u} (nach Ansicht der Ergebnisse gewählt, kein Beweis)."
                );
            }
            println!(
                "SmartMoney Nowcast-Backtest: am Markttag, nach der stündlichen METAR-Messung"
            );
            println!(
                "{} Events, {} Stationen. Modell: gemessenes Maximum/Minimum bis zur Entscheidung (harte Grenze) + \
                 Vorhersage der restlichen Stunden (Mittel aus 7 Modellen, mind. 24 h alt), korrigiert um den Fehler \
                 dieser Station zu dieser Uhrzeit aus den letzten {history_days} aufgelösten Tagen. Marktpreis: letzter \
                 Trade ab $2 in den 30 Minuten vor der Entscheidung. Gebühr 0,05 x p x (1-p), +{:.0} bps Slippage.",
                data.events.len(),
                stations.len(),
                cfg.backtest.slippage_bps
            );
            let mut results = Vec::new();
            for (hour, primary) in [(12u32, true), (10, false), (14, false), (16, false)] {
                let ncfg = nowcast::NowcastConfig {
                    decision_hour: hour,
                    history_days,
                    min_history,
                    sd_inflation,
                    sd_floor_f: 0.5,
                    sd_floor_c: 0.3,
                    min_price_usd: 2.0,
                    max_price_age: 30 * 60,
                };
                let obs = nowcast::observations(&data.events, &data.trades, &stations, &ncfg);
                let crit = weather_v2::criterion(
                    &obs,
                    true,
                    &[0.03, 0.05, 0.10, 0.15, 0.20],
                    200,
                    0.05,
                    cfg.backtest.slippage_bps,
                );
                println!(
                    "\n== Entscheidung {hour}:58 Ortszeit{}: {} Spannen mit frischem Preis ==",
                    if primary {
                        " (PRIMÄRER TEST, entscheidet)"
                    } else {
                        " (nur zur Information)"
                    },
                    obs.len()
                );
                println!("Kalibrierung (Gewinnquote je Wahrscheinlichkeitsbereich):");
                print!("{}", report::weather_calibration_table(&obs));
                println!();
                print!("{}", report::criterion_table(&crit));
                results.push(serde_json::json!({ "hour": hour, "primary": primary, "observations": obs.len(), "criterion": crit }));
            }
            ("weather_nowcast", serde_json::json!({ "results": results }))
        }
        Command::WeatherTruthCheck { days } => {
            let data = weather_load::load(&client, days, 60, now).await?;
            let stations = weather_load::station_data_v2(&client, &data.events, 60).await?;
            // key -> (matches, total)
            let mut by: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
            let mut off_by: std::collections::BTreeMap<i64, usize> = Default::default();
            for e in &data.events {
                let Some(st) = stations.get(&e.station) else {
                    continue;
                };
                let Some(&(max, min)) = st.obs.get(&e.date) else {
                    continue;
                };
                let v = match e.kind {
                    weather::Kind::High => max,
                    weather::Kind::Low => min,
                };
                let r = v.round() as i32;
                let Some(winner) = e.buckets.iter().find(|b| b.payout_yes >= 0.999) else {
                    continue;
                };
                let inside = |b: &weather::Bucket| {
                    b.range.lo.is_none_or(|l| r >= l) && b.range.hi.is_none_or(|h| r <= h)
                };
                let hit = inside(winner);
                // Distance in buckets between observed and winning bucket.
                let obs_idx = e.buckets.iter().position(inside);
                let win_idx = e.buckets.iter().position(|b| b.payout_yes >= 0.999);
                if let (Some(a), Some(b)) = (obs_idx, win_idx) {
                    *off_by.entry(a as i64 - b as i64).or_default() += 1;
                }
                let kind = match e.kind {
                    weather::Kind::High => "Hoch",
                    weather::Kind::Low => "Tief",
                };
                let unit = match e.unit {
                    weather::Unit::F => "°F",
                    weather::Unit::C => "°C",
                };
                for key in [
                    "Alle".to_owned(),
                    format!("Art {kind}"),
                    format!("Einheit {unit}"),
                    format!("Monat {}", e.date.format("%Y-%m")),
                    format!("Station {}", e.station),
                ] {
                    let x = by.entry(key).or_default();
                    x.1 += 1;
                    if hit {
                        x.0 += 1;
                    }
                }
            }
            println!("Diagnose: fällt die gemessene Tagesextremtemperatur (IEM, reguläre METAR) in die Gewinner-Spanne?");
            println!(
                "{:<18} {:>8} {:>8} {:>9}",
                "Gruppe", "Events", "Treffer", "Quote"
            );
            let mut stations_rows: Vec<(String, (usize, usize))> = Vec::new();
            for (k, (hit, n)) in &by {
                if k.starts_with("Station") {
                    stations_rows.push((k.clone(), (*hit, *n)));
                    continue;
                }
                println!(
                    "{:<18} {:>8} {:>8} {:>8.1}%",
                    k,
                    n,
                    hit,
                    100.0 * *hit as f64 / *n as f64
                );
            }
            stations_rows.sort_by(|a, b| {
                let qa = a.1 .0 as f64 / a.1 .1 as f64;
                let qb = b.1 .0 as f64 / b.1 .1 as f64;
                qa.total_cmp(&qb)
            });
            println!("\nSchwächste Stationen:");
            for (k, (hit, n)) in stations_rows.iter().take(10) {
                println!(
                    "{:<18} {:>8} {:>8} {:>8.1}%",
                    k,
                    n,
                    hit,
                    100.0 * *hit as f64 / *n as f64
                );
            }
            println!(
                "\nAbweichung in Spannen (gemessen minus Gewinner): {:?}",
                off_by
            );
            (
                "weather_truth_check",
                serde_json::json!({ "groups": by.iter().map(|(k, v)| (k.clone(), v.0, v.1)).collect::<Vec<_>>() }),
            )
        }
        Command::WeatherReaction { days } => {
            let data = weather_load::load(&client, days, 60, now).await?;
            let stations = weather_load::station_data_v2(&client, &data.events, 60).await?;
            let rcfg = reaction::ReactionConfig::default();
            let dead = reaction::dead_buckets(&data.events, &data.trades, &stations, &rcfg);
            println!(
                "Reaktions-Diagnose: Spannen, die eine reguläre METAR-Messung unmöglich gemacht hat \
                 (Maximum über der Obergrenze bzw. Minimum unter der Untergrenze, Rundung + Sicherheitsabstand).\n\
                 Abstand = Zeit nach der Messung (US: Minute :51, öffentlich ca. 3 min später). Gezählt werden \
                 Trades mit YES-Preis >= {:.0}¢. Abräumer = Taker, die YES verkaufen bzw. NO kaufen.",
                rcfg.stale_price * 100.0
            );
            let surprise =
                |d: &&reaction::DeadBucket| d.pre_price.is_some_and(|p| p >= rcfg.surprise_price);
            let unit_of = |d: &&reaction::DeadBucket| data.events[d.event].unit;
            let groups: Vec<(String, Vec<&reaction::DeadBucket>)> = vec![
                ("Alle".into(), dead.iter().collect()),
                (
                    "Alle, ohne trotzdem gewonnene".into(),
                    dead.iter().filter(|d| !d.won).collect(),
                ),
                (
                    "Alle, US (°F)".into(),
                    dead.iter()
                        .filter(|d| unit_of(d) == weather::Unit::F)
                        .collect(),
                ),
                (
                    format!(
                        "Überraschend (Preis davor >= {:.0}¢)",
                        rcfg.surprise_price * 100.0
                    ),
                    dead.iter().filter(surprise).collect(),
                ),
                (
                    "Überraschend, US (°F)".into(),
                    dead.iter()
                        .filter(surprise)
                        .filter(|d| unit_of(d) == weather::Unit::F)
                        .collect(),
                ),
                (
                    "Überraschend, international (°C)".into(),
                    dead.iter()
                        .filter(surprise)
                        .filter(|d| unit_of(d) == weather::Unit::C)
                        .collect(),
                ),
            ];
            let mut summaries = Vec::new();
            for (name, list) in &groups {
                let s = reaction::summarize(name, list, &data.events, &data.trades, &rcfg);
                print!("{}", reaction::table(&s));
                summaries.push(s);
            }
            (
                "weather_reaction",
                serde_json::json!({ "groups": summaries }),
            )
        }
        Command::Anchor {
            min_trade_usd,
            window_minutes,
            cutoff_minutes,
        } => {
            let data = anchor_load::load(
                &client,
                min_trade_usd,
                window_minutes * 60,
                cutoff_minutes * 60,
            )
            .await?;
            let s = &data.stats;
            println!(
                "SmartMoney Anker-Backtest: Polymarket-Fußball (Sieg/Remis/Sieg) gegen die entvigte \
                 Schlussquote (Pinnacle > Betfair-Börse > Schlussschnitt, football-data.co.uk)."
            );
            println!(
                "Marktpreis: letzter Trade >= ${min_trade_usd} im Fenster {window_minutes}-{cutoff_minutes} min \
                 vor Anpfiff. Gebühr 0,05 x p x (1-p), +{:.0} bps Slippage.",
                cfg.backtest.slippage_bps
            );
            println!(
                "Daten: {} Quotenzeilen, {} Events gesichtet, {} Moneyline-Spiele, {} ohne Auflösung.",
                s.odds_rows, s.events_seen, s.moneyline_events, s.unresolved
            );
            println!(
                "Join: {} verbunden, {} ohne Gegenstück, {} mehrdeutig (verworfen). Quellen: {:?}",
                s.joined, s.unmatched, s.ambiguous, s.sources
            );
            let mm_rate = if s.joined + s.truth_mismatch > 0 {
                100.0 * s.truth_mismatch as f64 / (s.joined + s.truth_mismatch) as f64
            } else {
                0.0
            };
            println!(
                "Wahrheitstest: {} Widersprüche Polymarket vs. CSV-Endstand ({mm_rate:.1} %, ausgeschlossen; > 2 % blockiert den Test).",
                s.truth_mismatch
            );
            println!(
                "Preise: {} von {} Ausgangs-Märkten mit frischem Preis ({} Abbrüche am Seitenlimit).",
                s.priced_outcomes, s.outcomes_total, s.paging_capped
            );
            if mm_rate > 2.0 {
                println!("ABBRUCH laut Kriterium: zu viele Wahrheits-Widersprüche; zuerst den Join prüfen.");
            }
            let obs = anchor::observations(&data.matches, &data.prices);
            println!("\nJe Liga (informativ): Brier Anker vs. Markt, Vorteil > 0 = Anker genauer");
            for (slug, _) in anchor::LEAGUES {
                let sub: Vec<weather::Observation> =
                    obs.iter().filter(|o| o.station == slug).cloned().collect();
                if let Some(c) = weather::compare(&sub) {
                    println!(
                        "{:<12} {:>5} Spannen in {:>4} Spielen: {:.4} vs {:.4}, Vorteil {:+.4} ({:+.4}..{:+.4})",
                        slug,
                        c.observations,
                        c.events,
                        c.brier_model,
                        c.brier_market,
                        c.advantage,
                        c.advantage_lo,
                        c.advantage_hi
                    );
                }
            }
            println!("\nKalibrierung (Gewinnquote je Wahrscheinlichkeitsbereich):");
            print!("{}", report::weather_calibration_table(&obs));
            let crit = weather_v2::criterion(
                &obs,
                true,
                &[0.02, 0.03, 0.05, 0.08, 0.10],
                30,
                0.05,
                cfg.backtest.slippage_bps,
            );
            println!();
            print!("{}", report::criterion_table(&crit));
            (
                "anchor",
                serde_json::json!({
                    "stats": s,
                    "observations": obs.len(),
                    "criterion": crit,
                    "params": {
                        "min_trade_usd": min_trade_usd,
                        "window_minutes": window_minutes,
                        "cutoff_minutes": cutoff_minutes,
                    },
                }),
            )
        }
    };

    finish(&common, &cfg, now, kind, output).await
}

/// Write the JSON file (if requested) and store the run in the database.
async fn finish(
    common: &Common,
    cfg: &Config,
    now: i64,
    kind: &str,
    output: serde_json::Value,
) -> anyhow::Result<()> {
    let output = serde_json::json!({
        "kind": kind,
        "created_at": now,
        "config": { "backtest": cfg.backtest, "scoring": cfg.scoring },
        "output": output,
    });
    if let Some(path) = &common.json {
        std::fs::write(path, serde_json::to_string_pretty(&output)?)
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("JSON written to {}", path.display());
    }
    if !common.no_save {
        let store = Store::open(&cfg.database.path).await?;
        sqlx::query(
            "INSERT INTO backtest_runs (created_at, params_json, results_json) VALUES (?, ?, ?)",
        )
        .bind(now)
        .bind(serde_json::json!({ "kind": kind, "config": output["config"] }).to_string())
        .bind(output["output"].to_string())
        .execute(store.pool())
        .await?;
        store.pool().close().await;
    }
    Ok(())
}
