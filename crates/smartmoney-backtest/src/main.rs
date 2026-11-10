//! `smartmoney-backtest`: replay resolved markets.
//!
//! - `score` (default): compare score variants on a broad market sample.
//! - `insider`: targeted test on personnel / corporate / legal markets.
//! - `smart`: do wallets that beat the market keep doing so?

use anyhow::Context;
use clap::{Args as ClapArgs, Parser, Subcommand};
use smartmoney_backtest::dataset::{self, Dataset, LoadOptions};
use smartmoney_backtest::fullhistory::{self, FullHistoryOptions};
use smartmoney_backtest::smart::{self, SmartConfig};
use smartmoney_backtest::{
    insider_variants, report, run_given_variants, run_variants, VariantResult,
};
use smartmoney_collector::PolymarketClient;
use smartmoney_core::{Config, MarketCategory};
use smartmoney_store::Store;
use std::collections::HashSet;
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
            cfg.backtest.markets_per_niche_tag = markets_per_tag_month;
            cfg.backtest.markets_per_control_tag = markets_per_tag_month;
            cfg.backtest.lookback_days = lookback as u64;
            // 30-day end-date windows over the whole range: an even sample
            // over time instead of only the most recently ended markets.
            let windows: Vec<(i64, i64)> = (0..(lookback + 29) / 30)
                .map(|k| (now - (k + 1) * 30 * DAY, now - k * 30 * DAY))
                .collect();
            let opts = LoadOptions {
                categories: None,
                wallets: false,
                max_event_pages: 40,
                windows: Some(windows),
            };
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
    };

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
