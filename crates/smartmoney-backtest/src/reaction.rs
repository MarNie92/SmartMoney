//! Reaction speed of weather markets (Phase 1 feasibility of a real-time bot).
//!
//! A bucket becomes "dead" when a routine METAR pushes the observed daily
//! extreme past it (highest-temperature markets: observed max above the
//! bucket's upper edge; lowest: observed min below the lower edge). From then
//! on its YES side cannot win. This module measures how long dead buckets
//! keep trading at non-trivial YES prices after the observation, who takes
//! the stale quotes (short-YES takers = "harvesters") and how much money that
//! race is worth. Observation times are the METAR measurement minute (:51 at
//! US stations); public APIs show the report about 3 minutes later.

use crate::nowcast::to_utc;
use crate::weather::{BucketTrade, Kind, Unit, WeatherEvent};
use crate::weather_v2::StationData;
use chrono_tz::Tz;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub struct ReactionConfig {
    /// Safety margin beyond the rounding edge (°F) before a bucket counts as dead.
    pub margin_f: f64,
    /// Same for °C markets (whole-degree METARs: 0.5 means one full degree past).
    pub margin_c: f64,
    /// Window before the observation for the "price before" (seconds).
    pub pre_window: i64,
    /// Minimum trade size (USD) for the "price before".
    pub min_usd: f64,
    /// YES price at or above which a trade in a dead bucket counts as stale.
    pub stale_price: f64,
    /// "Surprise" deaths: price before at or above this.
    pub surprise_price: f64,
}

impl Default for ReactionConfig {
    fn default() -> Self {
        Self {
            margin_f: 0.3,
            margin_c: 0.5,
            pre_window: 3600,
            min_usd: 2.0,
            stale_price: 0.02,
            surprise_price: 0.10,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeadBucket {
    pub event: usize,
    pub bucket: usize,
    /// UTC timestamp of the METAR that killed the bucket.
    pub obs_ts: i64,
    /// Last YES price (trade >= min_usd) within `pre_window` before obs_ts.
    pub pre_price: Option<f64>,
    /// Sanity check: the bucket won anyway (should be ~never).
    pub won: bool,
}

/// Whether the running extreme rules the bucket out.
pub fn is_dead(kind: Kind, lo: Option<i32>, hi: Option<i32>, running: f64, margin: f64) -> bool {
    match kind {
        Kind::High => hi.is_some_and(|h| running >= f64::from(h) + 0.5 + margin),
        Kind::Low => lo.is_some_and(|l| running <= f64::from(l) - 0.5 - margin),
    }
}

/// All buckets killed by an hourly observation during their event day.
pub fn dead_buckets(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    stations: &HashMap<String, StationData>,
    cfg: &ReactionConfig,
) -> Vec<DeadBucket> {
    let mut out = Vec::new();
    for (ei, e) in events.iter().enumerate() {
        let Some(st) = stations.get(&e.station) else {
            continue;
        };
        let Ok(tz) = st.timezone.parse::<Tz>() else {
            continue;
        };
        let margin = match e.unit {
            Unit::F => cfg.margin_f,
            Unit::C => cfg.margin_c,
        };
        let mut dead = vec![false; e.buckets.len()];
        let mut running: Option<f64> = None;
        for (t, v) in st.hourly_obs.iter().filter(|(t, _)| t.date() == e.date) {
            running = Some(match (running, e.kind) {
                (None, _) => *v,
                (Some(r), Kind::High) => r.max(*v),
                (Some(r), Kind::Low) => r.min(*v),
            });
            let r = running.unwrap();
            for (bi, b) in e.buckets.iter().enumerate() {
                if dead[bi] || !is_dead(e.kind, b.range.lo, b.range.hi, r, margin) {
                    continue;
                }
                dead[bi] = true;
                let Some(obs_ts) = to_utc(&tz, *t) else {
                    continue;
                };
                let pre_price = trades.get(&b.condition_id).and_then(|list| {
                    list.iter()
                        .filter(|x| {
                            x.usd >= cfg.min_usd
                                && x.timestamp < obs_ts
                                && x.timestamp >= obs_ts - cfg.pre_window
                        })
                        .max_by_key(|x| x.timestamp)
                        .map(|x| x.yes_price)
                });
                out.push(DeadBucket {
                    event: ei,
                    bucket: bi,
                    obs_ts,
                    pre_price,
                    won: b.payout_yes >= 0.999,
                });
            }
        }
    }
    out
}

/// Delay bins relative to the observation (seconds; upper bound exclusive).
pub const DELAY_BINS: [(&str, i64, i64); 9] = [
    ("vor Messung", -600, 0),
    ("0–1 min", 0, 60),
    ("1–2 min", 60, 120),
    ("2–3 min", 120, 180),
    ("3–5 min", 180, 300),
    ("5–10 min", 300, 600),
    ("10–30 min", 600, 1800),
    ("30–60 min", 1800, 3600),
    ("> 60 min", 3600, i64::MAX),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DelayRow {
    pub label: String,
    /// Short-YES takers at a stale price (they harvest the stale bids).
    pub harvest_trades: usize,
    pub harvest_usd: f64,
    /// Lower bound of the harvesters' profit (shares x YES price).
    pub harvest_profit: f64,
    /// Long-YES takers at a stale price (they buy a dead outcome).
    pub dead_buys: usize,
    pub dead_buy_usd: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReactionSummary {
    pub group: String,
    pub buckets: usize,
    pub won_anyway: usize,
    /// Median seconds from observation to the last stale trade (buckets that had one).
    pub median_last_stale: Option<i64>,
    /// Share of buckets with a stale trade later than 5 minutes after the observation.
    pub share_stale_after_5min: f64,
    pub rows: Vec<DelayRow>,
    /// Number of distinct event days in the group (for per-day figures).
    pub days: usize,
}

/// Lower bound for shares x YES price of a short-YES trade: `usd` is
/// shares x traded price, and the traded side is either YES at `p`
/// (profit = usd) or NO at `1 - p` (profit = usd * p / (1 - p)).
fn profit_lower_bound(t: &BucketTrade) -> f64 {
    let p = t.yes_price;
    if p >= 1.0 {
        return t.usd;
    }
    t.usd.min(t.usd * p / (1.0 - p))
}

pub fn summarize(
    group: &str,
    dead: &[&DeadBucket],
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    cfg: &ReactionConfig,
) -> ReactionSummary {
    let mut rows: Vec<DelayRow> = DELAY_BINS
        .iter()
        .map(|(l, _, _)| DelayRow {
            label: (*l).to_owned(),
            ..Default::default()
        })
        .collect();
    let mut last_stale: Vec<i64> = Vec::new();
    let mut after_5 = 0usize;
    let mut days = std::collections::HashSet::new();
    for d in dead {
        let e = &events[d.event];
        days.insert((e.station.clone(), e.date));
        let b = &e.buckets[d.bucket];
        let Some(list) = trades.get(&b.condition_id) else {
            continue;
        };
        let mut last: Option<i64> = None;
        for t in list {
            let delay = t.timestamp - d.obs_ts;
            if t.yes_price < cfg.stale_price {
                continue;
            }
            if delay >= 0 {
                last = Some(last.map_or(delay, |l: i64| l.max(delay)));
            }
            let Some(i) = DELAY_BINS
                .iter()
                .position(|(_, lo, hi)| delay >= *lo && delay < *hi)
            else {
                continue;
            };
            let r = &mut rows[i];
            if t.direction < 0 {
                r.harvest_trades += 1;
                r.harvest_usd += t.usd;
                r.harvest_profit += profit_lower_bound(t);
            } else {
                r.dead_buys += 1;
                r.dead_buy_usd += t.usd;
            }
        }
        if let Some(l) = last {
            last_stale.push(l);
            if l >= 300 {
                after_5 += 1;
            }
        }
    }
    last_stale.sort_unstable();
    ReactionSummary {
        group: group.to_owned(),
        buckets: dead.len(),
        won_anyway: dead.iter().filter(|d| d.won).count(),
        median_last_stale: last_stale.get(last_stale.len() / 2).copied(),
        share_stale_after_5min: if dead.is_empty() {
            0.0
        } else {
            after_5 as f64 / dead.len() as f64
        },
        rows,
        days: days.len(),
    }
}

fn fmt_delay(s: Option<i64>) -> String {
    match s {
        None => "–".into(),
        Some(s) if s < 120 => format!("{s} s"),
        Some(s) => format!("{:.1} min", s as f64 / 60.0),
    }
}

pub fn table(s: &ReactionSummary) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\n{}: {} tote Spannen an {} Event-Tagen, trotzdem gewonnen: {}, Median letzter Trade >= Schwelle: {}, \
         noch nach 5 min gehandelt: {:.0} %",
        s.group,
        s.buckets,
        s.days,
        s.won_anyway,
        fmt_delay(s.median_last_stale),
        s.share_stale_after_5min * 100.0
    );
    let header = format!(
        "{:<12} {:>9} {:>11} {:>12} {:>9} {:>11}",
        "Abstand", "Abräumer", "USD", "Gewinn min", "Käufer", "USD"
    );
    let _ = writeln!(out, "{header}");
    let _ = writeln!(out, "{}", "-".repeat(header.chars().count()));
    let mut total = 0.0;
    for r in &s.rows {
        total += r.harvest_profit;
        let _ = writeln!(
            out,
            "{:<12} {:>9} {:>11.0} {:>12.0} {:>9} {:>11.0}",
            r.label, r.harvest_trades, r.harvest_usd, r.harvest_profit, r.dead_buys, r.dead_buy_usd
        );
    }
    let per_day = if s.days > 0 {
        total / s.days as f64
    } else {
        0.0
    };
    let _ = writeln!(
        out,
        "Gewinn der Abräumer gesamt (Untergrenze): ${total:.0}, pro Event-Tag ${per_day:.2}"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weather::{Bucket, Range};
    use chrono::NaiveDate;

    #[test]
    fn dead_rules_respect_rounding_and_margin() {
        // Highest temp, bucket 70-71 °F: dead once the max rounds to 72+.
        assert!(!is_dead(Kind::High, Some(70), Some(71), 71.7, 0.3));
        assert!(is_dead(Kind::High, Some(70), Some(71), 71.8, 0.3));
        // Open upper bucket never dies in a high market.
        assert!(!is_dead(Kind::High, Some(90), None, 120.0, 0.3));
        // Lowest temp, bucket 10 °C (whole degrees): dead at 9 or below.
        assert!(!is_dead(Kind::Low, Some(10), Some(10), 10.0, 0.5));
        assert!(is_dead(Kind::Low, Some(10), Some(10), 9.0, 0.5));
    }

    fn station() -> StationData {
        let day = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        StationData {
            timezone: "America/Chicago".into(),
            hourly_obs: vec![
                (day.and_hms_opt(9, 51, 0).unwrap(), 70.0),
                (day.and_hms_opt(10, 51, 0).unwrap(), 72.1),
                (day.and_hms_opt(11, 51, 0).unwrap(), 74.0),
            ],
            ..Default::default()
        }
    }

    fn bucket(id: &str, lo: i32, hi: i32, won: bool) -> Bucket {
        Bucket {
            condition_id: id.into(),
            label: format!("{lo}-{hi}"),
            range: Range {
                lo: Some(lo),
                hi: Some(hi),
            },
            yes_index: 0,
            payout_yes: if won { 1.0 } else { 0.0 },
        }
    }

    fn trade(ts: i64, p: f64, usd: f64, dir: i8) -> BucketTrade {
        BucketTrade {
            timestamp: ts,
            yes_price: p,
            usd,
            wallet: 0,
            direction: dir,
        }
    }

    #[test]
    fn finds_deaths_and_measures_delays() {
        let day = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let event = WeatherEvent {
            event_id: "e".into(),
            kind: Kind::High,
            station: "KORD".into(),
            date: day,
            unit: Unit::F,
            buckets: vec![
                bucket("a", 68, 69, false),
                bucket("b", 70, 71, false),
                bucket("c", 72, 73, false),
                bucket("d", 74, 75, true),
            ],
        };
        // 10:51 CDT = 15:51 UTC.
        let obs_10 = to_utc(
            &"America/Chicago".parse::<Tz>().unwrap(),
            day.and_hms_opt(10, 51, 0).unwrap(),
        )
        .unwrap();
        let trades = HashMap::from([
            (
                "b".to_owned(),
                vec![
                    trade(obs_10 - 600, 0.30, 50.0, 1),
                    trade(obs_10 + 150, 0.25, 20.0, -1),
                    trade(obs_10 + 400, 0.05, 10.0, 1),
                    trade(obs_10 + 500, 0.01, 10.0, -1),
                ],
            ),
            ("a".to_owned(), vec![]),
        ]);
        let stations = HashMap::from([("KORD".to_owned(), station())]);
        let cfg = ReactionConfig::default();
        let dead = dead_buckets(std::slice::from_ref(&event), &trades, &stations, &cfg);
        // 09:51 70.0 kills 68-69; 10:51 72.1 kills 70-71; 11:51 74.0 kills 72-73.
        let ids: Vec<&str> = dead
            .iter()
            .map(|d| event.buckets[d.bucket].condition_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
        let b = &dead[1];
        assert_eq!(b.obs_ts, obs_10);
        assert_eq!(b.pre_price, Some(0.30));
        assert!(!b.won);

        let refs: Vec<&DeadBucket> = dead.iter().collect();
        let s = summarize("t", &refs, &[event], &trades, &cfg);
        assert_eq!(s.buckets, 3);
        assert_eq!(s.days, 1);
        // Harvest at +150 s (2-3 min bin): short YES at 0.25, $20.
        let r = &s.rows[3];
        assert_eq!(r.harvest_trades, 1);
        // Lower bound: min(20, 20 * 0.25 / 0.75).
        assert!((r.harvest_profit - 20.0 / 3.0).abs() < 1e-9);
        // Dead buy at +400 s (5-10 min bin); the 1¢ trade is below the stale price.
        assert_eq!(s.rows[5].dead_buys, 1);
        assert_eq!(s.median_last_stale, Some(400));
        // The trade 10 min before the observation lands in the "before" bin as a long.
        assert_eq!(s.rows[0].dead_buys, 1);
    }
}
