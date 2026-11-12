//! Aggregation per score bucket / category and text tables.

use crate::engine::Evaluated;
use serde::Serialize;
use smartmoney_core::MarketCategory;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FollowStats {
    pub delay_minutes: u64,
    /// Trades with a possible follow entry.
    pub filled: usize,
    pub avg_roi: Option<f64>,
    /// Profit with a fixed stake per filled trade.
    pub pnl_usd: f64,
    pub avg_move_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupStats {
    pub label: String,
    pub n: usize,
    /// Distinct markets and wallets (small numbers = correlated sample).
    pub markets: usize,
    pub wallets: usize,
    /// Average payout of the bought outcome (1 = won).
    pub hit_rate: Option<f64>,
    /// Average entry price = market-implied win probability.
    pub avg_price: Option<f64>,
    /// hit_rate - avg_price; > 0 means the group beat the market.
    pub edge: Option<f64>,
    /// Average return of the original trade per dollar.
    pub insider_roi: Option<f64>,
    pub follow: Vec<FollowStats>,
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

pub fn group_stats(label: &str, rows: &[&Evaluated], delays: &[u64], stake: f64) -> GroupStats {
    let hit_rate = mean(rows.iter().map(|r| r.payout));
    let avg_price = mean(rows.iter().map(|r| r.price));
    let follow = delays
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let fills: Vec<_> = rows.iter().filter_map(|r| r.follow[i].as_ref()).collect();
            FollowStats {
                delay_minutes: *d,
                filled: fills.len(),
                avg_roi: mean(fills.iter().map(|f| f.roi)),
                pnl_usd: fills.iter().map(|f| f.roi * stake).sum(),
                avg_move_pct: mean(fills.iter().map(|f| f.move_pct)),
            }
        })
        .collect();
    GroupStats {
        label: label.to_owned(),
        n: rows.len(),
        markets: rows
            .iter()
            .map(|r| r.condition_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        wallets: rows
            .iter()
            .map(|r| r.wallet.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        hit_rate,
        avg_price,
        edge: hit_rate.zip(avg_price).map(|(h, p)| h - p),
        insider_roi: mean(rows.iter().map(|r| r.payout / r.price - 1.0)),
        follow,
    }
}

/// Buckets `[0, b0)`, `[b0, b1)`, ..., the last one inclusive of its upper
/// bound, plus a final "Alle" row.
pub fn by_score(rows: &[Evaluated], bounds: &[f64], delays: &[u64], stake: f64) -> Vec<GroupStats> {
    let mut out = Vec::new();
    let mut lower = 0.0;
    for (i, upper) in bounds.iter().enumerate() {
        let last = i + 1 == bounds.len();
        let members: Vec<&Evaluated> = rows
            .iter()
            .filter(|r| r.score >= lower && (r.score < *upper || (last && r.score <= *upper)))
            .collect();
        out.push(group_stats(
            &format!("{lower:.0}–{upper:.0}"),
            &members,
            delays,
            stake,
        ));
        lower = *upper;
    }
    let all: Vec<&Evaluated> = rows.iter().collect();
    out.push(group_stats("Alle", &all, delays, stake));
    out
}

pub fn category_label(c: MarketCategory) -> &'static str {
    match c {
        MarketCategory::Personnel => "Personalie",
        MarketCategory::Corporate => "Firma",
        MarketCategory::Legal => "Gericht",
        MarketCategory::Politics => "Politik",
        MarketCategory::Other => "Sonstiges",
        MarketCategory::Sports => "Sport",
        MarketCategory::CryptoPrice => "Krypto-Kurs",
    }
}

/// Stats per market category for trades with a score of at least `min_score`.
pub fn by_category(
    rows: &[Evaluated],
    min_score: f64,
    delays: &[u64],
    stake: f64,
) -> Vec<GroupStats> {
    MarketCategory::ALL
        .iter()
        .filter_map(|c| {
            let members: Vec<&Evaluated> = rows
                .iter()
                .filter(|r| r.category == *c && r.score >= min_score)
                .collect();
            (!members.is_empty()).then(|| group_stats(category_label(*c), &members, delays, stake))
        })
        .collect()
}

fn pct(v: Option<f64>) -> String {
    v.map_or("–".into(), |v| format!("{:.1}%", v * 100.0))
}

fn signed_pct(v: Option<f64>) -> String {
    v.map_or("–".into(), |v| format!("{:+.1}%", v * 100.0))
}

/// Render a table. `follow_index` selects the delay shown for "Bewegung".
pub fn table(title: &str, groups: &[GroupStats]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\n{title}");
    let delays: Vec<u64> = groups
        .first()
        .map(|g| g.follow.iter().map(|f| f.delay_minutes).collect())
        .unwrap_or_default();
    let mut header = format!(
        "{:<11} {:>6} {:>7} {:>8} {:>8} {:>8} {:>8} {:>10}",
        "Bucket", "n", "Märkte", "Wallets", "Treffer", "Ø Kurs", "Edge", "ROI Ins."
    );
    for d in &delays {
        let _ = write!(header, " {:>10}", format!("ROI +{d}m"));
    }
    if let Some(d) = delays.get(delays.len().saturating_sub(2)) {
        let _ = write!(header, " {:>11}", format!("Bew. +{d}m"));
    }
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for g in groups {
        let mut line = format!(
            "{:<11} {:>6} {:>7} {:>8} {:>8} {:>8} {:>8} {:>10}",
            g.label,
            g.n,
            g.markets,
            g.wallets,
            pct(g.hit_rate),
            pct(g.avg_price),
            signed_pct(g.edge),
            signed_pct(g.insider_roi),
        );
        for f in &g.follow {
            let cell = match f.avg_roi {
                Some(r) => format!("{:+.1}% ({})", r * 100.0, f.filled),
                None => "–".into(),
            };
            let _ = write!(line, " {cell:>10}");
        }
        if let Some(f) = g.follow.get(g.follow.len().saturating_sub(2)) {
            let cell = f.avg_move_pct.map_or("–".into(), |m| format!("{m:+.1}%"));
            let _ = write!(line, " {cell:>11}");
        }
        let _ = writeln!(s, "{line}");
    }
    s
}

/// Table for the smart-money persistence test.
pub fn smart_table(r: &crate::smart::SmartReport) -> String {
    let mut s = String::new();
    let delays: Vec<u64> = r
        .groups
        .first()
        .map(|g| g.copy.iter().map(|c| c.delay_minutes).collect())
        .unwrap_or_default();
    let mut header = format!(
        "{:<8} {:>8} {:>9} {:>9} {:>20} {:>10} {:>8} {:>7}",
        "Gruppe",
        "Wallets",
        "Edge P1",
        "Edge P2",
        "95%-Bereich P2",
        "P2 $-gew.",
        "Trades",
        "Märkte"
    );
    for d in &delays {
        let _ = write!(header, " {:>15}", format!("Kopie +{d}m"));
    }
    // Buys vs sells at the second-to-last delay (e.g. 15 min).
    let split_idx = delays.len().saturating_sub(2);
    if let Some(d) = delays.get(split_idx) {
        let _ = write!(
            header,
            " {:>13} {:>13}",
            format!("Käufe +{d}m"),
            format!("Verk. +{d}m")
        );
    }
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    let n = r.groups.len();
    for g in &r.groups {
        let label = match g.rank {
            1 => "Top".to_owned(),
            r if r == n => "Flop".to_owned(),
            r => format!("{r}."),
        };
        let mut line = format!(
            "{:<8} {:>8} {:>9} {:>9} {:>20} {:>10} {:>8} {:>7}",
            label,
            g.wallets,
            signed_pct(Some(g.p1_edge_avg)),
            signed_pct(Some(g.p2_edge_avg)),
            format!(
                "{} bis {}",
                signed_pct(Some(g.p2_edge_lo)),
                signed_pct(Some(g.p2_edge_hi))
            ),
            signed_pct(Some(g.p2_edge_usd_weighted)),
            g.p2_trades,
            g.p2_markets,
        );
        for c in &g.copy {
            let cell = match c.avg_roi {
                Some(v) => format!("{:+.1}% ({}/{}M)", v * 100.0, c.filled, c.markets),
                None => "–".into(),
            };
            let _ = write!(line, " {cell:>15}");
        }
        if let Some(c) = g.copy.get(split_idx) {
            let fmt = |v: Option<f64>, n: usize| {
                v.map_or("–".into(), |v| format!("{:+.1}% ({n})", v * 100.0))
            };
            let _ = write!(
                line,
                " {:>13} {:>13}",
                fmt(c.buy_avg_roi, c.buy_filled),
                fmt(c.sell_avg_roi, c.sell_filled)
            );
        }
        let _ = writeln!(s, "{line}");
    }
    s
}

/// Calibration table per price bucket.
pub fn calibration_table(rows: &[crate::calibration::CalibStats]) -> String {
    let mut s = String::new();
    let header = format!(
        "{:<10} {:>9} {:>7} {:>8} {:>8} {:>8} {:>20} {:>12}",
        "Kurs", "Trades", "Märkte", "Ø Kurs", "Treffer", "Edge", "95%-Bereich Edge", "Kauf-Rendite"
    );
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for r in rows {
        let _ = writeln!(
            s,
            "{:<10} {:>9} {:>7} {:>8} {:>8} {:>8} {:>20} {:>12}",
            r.label,
            r.trades,
            r.markets,
            pct(r.avg_price),
            pct(r.hit_rate),
            signed_pct(r.edge),
            format!("{} bis {}", signed_pct(r.edge_lo), signed_pct(r.edge_hi)),
            signed_pct(r.buy_roi),
        );
    }
    s
}

/// Edge per price zone for groups (category, time to resolution).
pub fn zone_table(title: &str, groups: &[crate::calibration::GroupCalib]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\n{title}");
    let mut header = format!("{:<13} {:>9} {:>7}", "Gruppe", "Trades", "Märkte");
    for (name, _, _) in crate::calibration::ZONES {
        let _ = write!(header, " {:>34}", format!("Edge {name} (95%)"));
    }
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for g in groups {
        let mut line = format!("{:<13} {:>9} {:>7}", g.label, g.trades, g.markets);
        for z in &g.zones {
            let cell = match z.edge {
                Some(e) => format!(
                    "{:+.1}% ({}..{}) n={}",
                    e * 100.0,
                    signed_pct(z.edge_lo),
                    signed_pct(z.edge_hi),
                    z.trades
                ),
                None => "–".into(),
            };
            let _ = write!(line, " {cell:>34}");
        }
        let _ = writeln!(s, "{line}");
    }
    s
}

fn usd_compact(v: f64) -> String {
    let a = v.abs();
    let sign = if v < 0.0 { "-" } else { "" };
    if a >= 1e6 {
        format!("{sign}${:.1} Mio", a / 1e6)
    } else if a >= 1e3 {
        format!("{sign}${:.0}k", a / 1e3)
    } else {
        format!("{sign}${a:.0}")
    }
}

/// Wallet group profiles: PnL composition and behaviour.
pub fn bots_table(groups: &[crate::bots::GroupProfile]) -> String {
    let mut s = String::new();
    let header = format!(
        "{:<28} {:>9} {:>10} {:>10} {:>11} {:>12} {:>11} {:>10} {:>10} {:>12} {:>8}",
        "Gruppe",
        "Wallets",
        "profitabel",
        "Median",
        "Gesamt",
        "Handel real.",
        "unrealis.",
        "Rebates",
        "Prämien",
        "Volumen",
        "Marge"
    );
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for g in groups {
        let _ = writeln!(
            s,
            "{:<28} {:>9} {:>10} {:>10} {:>11} {:>12} {:>11} {:>10} {:>10} {:>12} {:>8}",
            g.label,
            format!("{}/{}", g.with_pnl, g.wallets),
            pct(g.profitable_share),
            g.median_economic.map_or("–".into(), usd_compact),
            usd_compact(g.sum_economic),
            usd_compact(g.sum_trading_realized),
            usd_compact(g.sum_unrealized),
            usd_compact(g.sum_rebates),
            usd_compact(g.sum_sponsored),
            usd_compact(g.sum_volume_usdc),
            g.margin
                .map_or("–".into(), |m| format!("{:+.2}%", m * 100.0)),
        );
    }
    let header2 = format!(
        "{:<28} {:>8} {:>9} {:>9} {:>9} {:>9} {:>11} {:>8} {:>8} {:>9}",
        "Verhalten (Stichprobe)",
        "Wallets",
        "Trades",
        "Verkäufe",
        "bis 10¢",
        "ab 90¢",
        "beidseitig",
        "Sport",
        "Krypto",
        "Median"
    );
    let _ = writeln!(s, "\n{header2}");
    let _ = writeln!(s, "{}", "-".repeat(header2.chars().count()));
    for g in groups {
        let Some(b) = &g.behavior else {
            let _ = writeln!(s, "{:<28} {:>8}", g.label, 0);
            continue;
        };
        let _ = writeln!(
            s,
            "{:<28} {:>8} {:>9} {:>9} {:>9} {:>9} {:>11} {:>8} {:>8} {:>9}",
            g.label,
            g.with_behavior,
            b.trades,
            pct(Some(b.sell_share)),
            pct(Some(b.longshot_share)),
            pct(Some(b.favorite_share)),
            pct(Some(b.two_sided_share)),
            pct(Some(b.sports_share)),
            pct(Some(b.crypto_share)),
            usd_compact(b.median_usd),
        );
    }
    s
}

/// Status-quo strategy rows.
pub fn status_quo_table(title: &str, rows: &[crate::statusquo::SqResult]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\n{title}");
    let header = format!(
        "{:<20} {:>7} {:>7} {:>9} {:>9} {:>9} {:>20} {:>10} {:>10} {:>11}",
        "Regel",
        "Märkte",
        "Events",
        "Gewinn%",
        "Ø Kosten",
        "Rendite",
        "95%-Bereich",
        "Gewinn",
        "Drawdown",
        "schlechtest"
    );
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for r in rows {
        let _ = writeln!(
            s,
            "{:<20} {:>7} {:>7} {:>9} {:>9} {:>9} {:>20} {:>10} {:>10} {:>11}",
            r.label,
            r.markets,
            r.events,
            pct(Some(r.win_rate)),
            format!("{:.1}¢", r.avg_cost * 100.0),
            signed_pct(Some(r.roi)),
            format!(
                "{} bis {}",
                signed_pct(Some(r.roi_lo)),
                signed_pct(Some(r.roi_hi))
            ),
            usd_compact(r.pnl),
            usd_compact(-r.max_drawdown),
            usd_compact(r.worst_trade),
        );
    }
    s
}

/// Brier comparison rows (label, comparison).
pub fn weather_score_table(rows: &[(String, crate::weather::ScoreComparison)]) -> String {
    let mut s = String::new();
    let header = format!(
        "{:<22} {:>8} {:>7} {:>12} {:>12} {:>12} {:>24}",
        "Gruppe",
        "Spannen",
        "Events",
        "Brier Modell",
        "Brier Markt",
        "Vorteil",
        "95%-Bereich Vorteil"
    );
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for (label, c) in rows {
        let _ = writeln!(
            s,
            "{:<22} {:>8} {:>7} {:>12.4} {:>12.4} {:>+12.4} {:>24}",
            label,
            c.observations,
            c.events,
            c.brier_model,
            c.brier_market,
            c.advantage,
            format!("{:+.4} bis {:+.4}", c.advantage_lo, c.advantage_hi),
        );
    }
    s
}

/// Strategy rows per edge threshold.
pub fn weather_strategy_table(rows: &[crate::weather::StrategyResult]) -> String {
    let mut s = String::new();
    let header = format!(
        "{:<10} {:>7} {:>7} {:>9} {:>9} {:>12} {:>22} {:>13} {:>11}",
        "Schwelle",
        "Wetten",
        "Events",
        "Gewinn%",
        "Ø Kurs",
        "Taker-Rend.",
        "95%-Bereich Taker",
        "Maker-Rend.*",
        "Gewinn $10"
    );
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for r in rows {
        let _ = writeln!(
            s,
            "{:<10} {:>7} {:>7} {:>9} {:>9} {:>12} {:>22} {:>13} {:>11}",
            format!("{:.0} Pkt.", r.threshold * 100.0),
            r.bets,
            r.events,
            pct(Some(r.win_rate)),
            format!("{:.1}¢", r.avg_price * 100.0),
            signed_pct(Some(r.roi_taker)),
            format!(
                "{} bis {}",
                signed_pct(Some(r.roi_taker_lo)),
                signed_pct(Some(r.roi_taker_hi))
            ),
            signed_pct(Some(r.roi_maker)),
            usd_compact(r.pnl_taker),
        );
    }
    s
}

/// Calibration of model and market probabilities in bins.
pub fn weather_calibration_table(obs: &[crate::weather::Observation]) -> String {
    let bins = [0.0, 0.05, 0.15, 0.30, 0.50, 0.70, 0.85, 1.0001];
    let mut s = String::new();
    let header = format!(
        "{:<12} {:>9} {:>12} {:>10} {:>9} {:>12} {:>10}",
        "Wahrsch.", "n Modell", "Ø Modell", "Treffer", "n Markt", "Ø Markt", "Treffer"
    );
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for w in bins.windows(2) {
        let stats = |f: fn(&crate::weather::Observation) -> f64| {
            let sel: Vec<&crate::weather::Observation> =
                obs.iter().filter(|o| f(o) >= w[0] && f(o) < w[1]).collect();
            let n = sel.len();
            let avg = (n > 0).then(|| sel.iter().map(|o| f(o)).sum::<f64>() / n as f64);
            let hit = (n > 0).then(|| sel.iter().map(|o| o.outcome).sum::<f64>() / n as f64);
            (n, avg, hit)
        };
        let (nm, am, hm) = stats(|o| o.model);
        let (nk, ak, hk) = stats(|o| o.market);
        let _ = writeln!(
            s,
            "{:<12} {:>9} {:>12} {:>10} {:>9} {:>12} {:>10}",
            format!("{:.0}–{:.0} %", w[0] * 100.0, (w[1] * 100.0).min(100.0)),
            nm,
            pct(am),
            pct(hm),
            nk,
            pct(ak),
            pct(hk),
        );
    }
    s
}

fn copy_cell(c: &crate::weather_experts::CopyResult) -> String {
    match (c.roi, c.roi_lo, c.roi_hi) {
        (Some(r), Some(lo), Some(hi)) => format!(
            "{:+.1}% ({:+.0}..{:+.0}) n={}",
            r * 100.0,
            lo * 100.0,
            hi * 100.0,
            c.copies
        ),
        _ => "–".into(),
    }
}

/// Weather expert groups: persistence and copy returns.
pub fn experts_table(r: &crate::weather_experts::ExpertReport) -> String {
    let mut s = String::new();
    let delays: Vec<u64> = r
        .groups
        .first()
        .map(|g| g.copy.iter().map(|c| c.delay_minutes).collect())
        .unwrap_or_default();
    let mut header = format!(
        "{:<18} {:>8} {:>9} {:>9} {:>20} {:>9}",
        "Gruppe", "Wallets", "Edge P1", "Edge P2", "95%-Bereich P2", "Trades P2"
    );
    for d in &delays {
        let _ = write!(header, " {:>27}", format!("Kopie +{d} min"));
    }
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for g in &r.groups {
        let mut line = format!(
            "{:<18} {:>8} {:>9} {:>9} {:>20} {:>9}",
            g.label,
            g.wallets,
            signed_pct(Some(g.p1_edge)),
            signed_pct(Some(g.p2_edge)),
            format!(
                "{} bis {}",
                signed_pct(Some(g.p2_edge_lo)),
                signed_pct(Some(g.p2_edge_hi))
            ),
            g.p2_trades,
        );
        for c in &g.copy {
            let _ = write!(line, " {:>27}", copy_cell(c));
        }
        let _ = writeln!(s, "{line}");
    }
    s
}

/// One wallet: edge and copy returns over the whole period.
pub fn wallet_profile_table(p: &crate::weather_experts::WalletProfile) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "Wallet {}: {} Trades in {} Events, Umsatz {}, Edge {} (nach Einsatz gewichtet)",
        p.wallet,
        p.trades,
        p.events,
        usd_compact(p.usd),
        signed_pct(Some(p.edge))
    );
    for c in &p.copy {
        let _ = writeln!(
            s,
            "   Kopieren nach {:>2} min: {}",
            c.delay_minutes,
            copy_cell(c)
        );
    }
    s
}

/// Robustness grid of the top-N copy test.
pub fn robustness_table(rows: &[crate::weather_experts::RobustRow]) -> String {
    let mut s = String::new();
    let delays: Vec<u64> = rows
        .first()
        .map(|r| r.copy.iter().map(|c| c.delay_minutes).collect())
        .unwrap_or_default();
    let mut header = format!(
        "{:<11} {:>5} {:>9} {:>9} {:>20}",
        "Stichtag", "Top", "Slippage", "Edge P2", "95%-Bereich P2"
    );
    for d in &delays {
        let _ = write!(header, " {:>27}", format!("Kopie +{d} min"));
    }
    let _ = writeln!(s, "{header}");
    let _ = writeln!(s, "{}", "-".repeat(header.chars().count()));
    for r in rows {
        let mut line = format!(
            "{:<11} {:>5} {:>9} {:>9} {:>20}",
            r.split_date.to_string(),
            r.top_n,
            format!("{:.1}%", r.slippage_bps / 100.0),
            signed_pct(Some(r.p2_edge)),
            format!(
                "{} bis {}",
                signed_pct(Some(r.p2_edge_lo)),
                signed_pct(Some(r.p2_edge_hi))
            ),
        );
        for c in &r.copy {
            let _ = write!(line, " {:>27}", copy_cell(c));
        }
        let _ = writeln!(s, "{line}");
    }
    // Plateau summary per delay.
    let _ = writeln!(s);
    for (i, d) in delays.iter().enumerate() {
        let rois: Vec<f64> = rows.iter().filter_map(|r| r.copy[i].roi).collect();
        let positive = rois.iter().filter(|r| **r > 0.0).count();
        let significant = rows
            .iter()
            .filter(|r| r.copy[i].roi_lo.is_some_and(|lo| lo > 0.0))
            .count();
        let mut sorted = rois.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted.get(sorted.len() / 2).copied();
        let _ = writeln!(
            s,
            "Kopie +{d} min: {positive} von {} Varianten im Plus, {significant} mit 95%-Bereich über 0, Median {}, schlechteste {}",
            rois.len(),
            signed_pct(median),
            signed_pct(sorted.first().copied()),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::FollowResult;

    fn row(score: f64, price: f64, payout: f64, follow_roi: Option<f64>) -> Evaluated {
        Evaluated {
            condition_id: "0xm".into(),
            wallet: "w".into(),
            timestamp: 0,
            score,
            category: MarketCategory::Legal,
            control: false,
            price,
            usd: 1000.0,
            wallet_age_days: None,
            payout,
            follow: vec![follow_roi.map(|roi| FollowResult {
                entry: price,
                roi,
                move_pct: 10.0,
            })],
        }
    }

    #[test]
    fn edge_and_roi() {
        // Two longshots at 10¢, one wins: hit 50 % vs implied 10 %.
        let rows = [
            row(80.0, 0.1, 1.0, Some(8.0)),
            row(85.0, 0.1, 0.0, Some(-1.0)),
        ];
        let refs: Vec<&Evaluated> = rows.iter().collect();
        let g = group_stats("x", &refs, &[15], 100.0);
        assert_eq!(g.n, 2);
        assert_eq!(g.markets, 1);
        assert_eq!(g.wallets, 1);
        assert!((g.hit_rate.unwrap() - 0.5).abs() < 1e-9);
        assert!((g.edge.unwrap() - 0.4).abs() < 1e-9);
        assert!((g.insider_roi.unwrap() - 4.0).abs() < 1e-9); // (9 + -1) / 2
        assert_eq!(g.follow[0].filled, 2);
        assert!((g.follow[0].pnl_usd - 700.0).abs() < 1e-9);
    }

    #[test]
    fn buckets_cover_all_scores() {
        let rows = vec![
            row(0.0, 0.5, 1.0, None),
            row(19.9, 0.5, 1.0, None),
            row(20.0, 0.5, 0.0, None),
            row(100.0, 0.5, 1.0, None),
        ];
        let b = by_score(&rows, &[20.0, 60.0, 100.0], &[15], 100.0);
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].n, 2);
        assert_eq!(b[1].n, 1);
        assert_eq!(b[2].n, 1, "100 belongs to the last bucket");
        assert_eq!(b[3].label, "Alle");
        assert_eq!(b[3].n, 4);
        assert_eq!(b[0].follow[0].filled, 0);
        assert_eq!(b[0].follow[0].avg_roi, None);
    }

    #[test]
    fn categories_and_table_render() {
        let rows = vec![row(70.0, 0.2, 1.0, Some(3.0)), row(10.0, 0.2, 0.0, None)];
        let c = by_category(&rows, 60.0, &[15], 100.0);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].label, "Gericht");
        let t = table("Test", &by_score(&rows, &[50.0, 100.0], &[15], 100.0));
        assert!(t.contains("ROI +15m"));
        assert!(t.contains("Alle"));
        assert!(t.contains("+300.0% (1)"));
    }
}
