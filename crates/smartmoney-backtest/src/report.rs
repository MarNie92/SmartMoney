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
        "{:<11} {:>6} {:>8} {:>8} {:>8} {:>10}",
        "Bucket", "n", "Treffer", "Ø Kurs", "Edge", "ROI Ins."
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
            "{:<11} {:>6} {:>8} {:>8} {:>8} {:>10}",
            g.label,
            g.n,
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
