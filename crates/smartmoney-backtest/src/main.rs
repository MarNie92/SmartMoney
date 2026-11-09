//! `smartmoney-backtest`: replay resolved markets and compare score variants.

use anyhow::Context;
use clap::Parser;
use smartmoney_backtest::{dataset, report, run_variants};
use smartmoney_collector::PolymarketClient;
use smartmoney_core::Config;
use smartmoney_store::Store;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "smartmoney-backtest",
    about = "Replays resolved Polymarket markets and reports how high-score trades performed"
)]
struct Args {
    /// Markets per niche tag (overrides config).
    #[arg(long)]
    markets_per_niche_tag: Option<usize>,
    /// Markets per control tag (overrides config).
    #[arg(long)]
    markets_per_control_tag: Option<usize>,
    /// Only markets that ended within this many days (overrides config).
    #[arg(long)]
    lookback_days: Option<u64>,
    /// Requests per 10 seconds (overrides config).
    #[arg(long)]
    rps10: Option<u32>,
    /// Directory for cached API responses.
    #[arg(long, default_value = "backtest_cache")]
    cache_dir: PathBuf,
    /// Also write the results as JSON to this file.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Do not store the run in the database.
    #[arg(long)]
    no_save: bool,
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
    let args = Args::parse();

    let mut cfg = Config::load().context("loading configuration")?;
    if let Some(n) = args.markets_per_niche_tag {
        cfg.backtest.markets_per_niche_tag = n;
    }
    if let Some(n) = args.markets_per_control_tag {
        cfg.backtest.markets_per_control_tag = n;
    }
    if let Some(d) = args.lookback_days {
        cfg.backtest.lookback_days = d;
    }
    if let Some(r) = args.rps10 {
        cfg.backtest.requests_per_10s = r;
    }

    let client = PolymarketClient::new(&cfg.collector)?
        .with_rate(cfg.backtest.requests_per_10s)
        .with_cache(&args.cache_dir)
        .with_context(|| format!("creating cache dir {}", args.cache_dir.display()))?;
    let now = chrono::Utc::now().timestamp();
    let data = dataset::load(&client, &cfg.backtest, now).await?;
    let results = run_variants(&data, &cfg);

    let s = &data.stats;
    println!(
        "SmartMoney Backtest: {} Märkte ausgewertet ({} ausgewählt, {} ohne Auflösung, {} zu viele Trades, {} Fehler), {} Trades, {} Wallets.",
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
    println!(
        "Treffer = Anteil gewonnener Käufe. Ø Kurs = vom Markt eingepreiste Gewinnchance. \
         Edge = Treffer - Ø Kurs (> 0: besser als der Markt). ROI +Xm = Rendite beim Nachziehen X Minuten später \
         zum nächsten echten Trade (+{:.0} bps Slippage), in Klammern die Anzahl möglicher Einstiege.",
        cfg.backtest.slippage_bps
    );
    for r in &results {
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
        print!(
            "{}",
            report::table(
                &format!(
                    "   nach Kategorie (Score ab {:.0})",
                    cfg.scoring.alert_threshold
                ),
                &r.by_category
            )
        );
    }
    println!(
        "\nHinweise: Wallet-Alter aus join_date; die Anzahl gehandelter Märkte fließt nicht ein (nur heutiger Wert verfügbar). \
         Konzentration aus den Käufen der letzten {} Tage angenähert. Märkte mit mehr als {} Trades werden übersprungen.",
        cfg.backtest.wallet_history_days, cfg.backtest.max_trades_per_market
    );

    let output = serde_json::json!({
        "created_at": now,
        "stats": data.stats,
        "config": { "backtest": cfg.backtest, "scoring": cfg.scoring },
        "results": results,
    });
    if let Some(path) = &args.json {
        std::fs::write(path, serde_json::to_string_pretty(&output)?)
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("JSON written to {}", path.display());
    }
    if !args.no_save {
        let store = Store::open(&cfg.database.path).await?;
        sqlx::query(
            "INSERT INTO backtest_runs (created_at, params_json, results_json) VALUES (?, ?, ?)",
        )
        .bind(now)
        .bind(output["config"].to_string())
        .bind(
            serde_json::json!({"stats": output["stats"], "results": output["results"]}).to_string(),
        )
        .execute(store.pool())
        .await?;
        store.pool().close().await;
    }
    Ok(())
}
