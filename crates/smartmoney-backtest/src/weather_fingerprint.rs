//! Fingerprint of the best weather traders: when, how early and which
//! buckets they trade, compared with everyone else, and where their edge
//! comes from. The goal is to find the information they react to (model
//! runs, live observations, ...) so it can be used directly instead of
//! following them.

use crate::weather::{BucketTrade, WeatherEvent};
use crate::weather_experts::{bucket_index, trade_edge};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// One trade with the context needed for the fingerprint.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeContext {
    pub wallet: u32,
    pub timestamp: i64,
    pub usd: f64,
    pub edge: f64,
    pub yes_price: f64,
    pub direction: i8,
    /// Seconds from the start of the event's local day (negative = before).
    pub to_day_start: i64,
    /// Station UTC offset (seconds).
    pub utc_offset: i64,
    /// Bucket centre minus model mean, in model standard deviations.
    pub z: Option<f64>,
}

/// Collect trades with context. `offsets` maps station -> UTC offset;
/// `models` maps event index -> (mean, sd) of a forecast model.
pub fn contexts(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    offsets: &HashMap<String, i64>,
    models: &HashMap<usize, (f64, f64)>,
) -> Vec<TradeContext> {
    let buckets = bucket_index(events);
    // condition id -> bucket centre
    let centres: HashMap<&str, f64> = events
        .iter()
        .flat_map(|e| {
            e.buckets.iter().filter_map(move |b| {
                Some((
                    b.condition_id.as_str(),
                    crate::weather::bucket_value(b.range, e.unit)?,
                ))
            })
        })
        .collect();
    let mut out = Vec::new();
    for (cid, list) in trades {
        let Some(info) = buckets.get(cid.as_str()) else {
            continue;
        };
        let e = &events[info.event];
        let offset = offsets.get(&e.station).copied().unwrap_or(0);
        let day_start = e.date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - offset;
        let z = match (models.get(&info.event), centres.get(cid.as_str())) {
            (Some((mu, sd)), Some(c)) if *sd > 0.0 => Some((c - mu) / sd),
            _ => None,
        };
        for t in list {
            out.push(TradeContext {
                wallet: t.wallet,
                timestamp: t.timestamp,
                usd: t.usd,
                edge: trade_edge(t, info.payout_yes),
                yes_price: t.yes_price,
                direction: t.direction,
                to_day_start: t.timestamp - day_start,
                utc_offset: offset,
                z,
            });
        }
    }
    out
}

/// Best wallets by USD-weighted edge with minimum activity.
pub fn top_wallets(ctx: &[TradeContext], min_trades: usize, n: usize) -> Vec<(u32, f64, usize)> {
    let mut acc: HashMap<u32, (f64, f64, usize)> = HashMap::new();
    for c in ctx {
        let a = acc.entry(c.wallet).or_default();
        a.0 += c.usd * c.edge;
        a.1 += c.usd;
        a.2 += 1;
    }
    let mut ranked: Vec<(u32, f64, usize)> = acc
        .into_iter()
        .filter(|(_, a)| a.2 >= min_trades && a.1 > 0.0)
        .map(|(w, a)| (w, a.0 / a.1, a.2))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(n);
    ranked
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BinRow {
    pub label: String,
    /// Share of the top group's trades (by count) in this bin.
    pub share_top: f64,
    pub share_rest: f64,
    /// share_top / share_rest.
    pub lift: Option<f64>,
    /// USD-weighted edge of the top group's trades in this bin.
    pub edge_top: Option<f64>,
    pub edge_rest: Option<f64>,
    pub trades_top: usize,
}

/// Distribution over bins (`bin` returns an index into `labels`).
pub fn distribution(
    ctx: &[TradeContext],
    top: &HashSet<u32>,
    labels: &[String],
    bin: impl Fn(&TradeContext) -> Option<usize>,
) -> Vec<BinRow> {
    #[derive(Default, Clone)]
    struct A {
        n: usize,
        usd: f64,
        edge_usd: f64,
    }
    let mut top_acc = vec![A::default(); labels.len()];
    let mut rest_acc = vec![A::default(); labels.len()];
    let (mut n_top, mut n_rest) = (0usize, 0usize);
    for c in ctx {
        let Some(i) = bin(c) else { continue };
        let a = if top.contains(&c.wallet) {
            n_top += 1;
            &mut top_acc[i]
        } else {
            n_rest += 1;
            &mut rest_acc[i]
        };
        a.n += 1;
        a.usd += c.usd;
        a.edge_usd += c.usd * c.edge;
    }
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let st = if n_top > 0 {
                top_acc[i].n as f64 / n_top as f64
            } else {
                0.0
            };
            let sr = if n_rest > 0 {
                rest_acc[i].n as f64 / n_rest as f64
            } else {
                0.0
            };
            BinRow {
                label: l.clone(),
                share_top: st,
                share_rest: sr,
                lift: (sr > 0.0).then(|| st / sr),
                edge_top: (top_acc[i].usd > 0.0).then(|| top_acc[i].edge_usd / top_acc[i].usd),
                edge_rest: (rest_acc[i].usd > 0.0).then(|| rest_acc[i].edge_usd / rest_acc[i].usd),
                trades_top: top_acc[i].n,
            }
        })
        .collect()
}

pub fn utc_hour(c: &TradeContext) -> Option<usize> {
    Some(c.timestamp.rem_euclid(86_400) as usize / 3600)
}

pub fn local_hour(c: &TradeContext) -> Option<usize> {
    Some((c.timestamp + c.utc_offset).rem_euclid(86_400) as usize / 3600)
}

/// Five-minute slot within the hour (0..12).
pub fn minute_slot(c: &TradeContext) -> Option<usize> {
    Some(c.timestamp.rem_euclid(3600) as usize / 300)
}

pub const DAY_BINS: [(&str, i64, i64); 9] = [
    ("> 36 h vorher", i64::MIN, -36 * 3600),
    ("36–24 h vorher", -36 * 3600, -24 * 3600),
    ("24–12 h vorher", -24 * 3600, -12 * 3600),
    ("12–0 h vorher", -12 * 3600, 0),
    ("Tag 0–6 Uhr", 0, 6 * 3600),
    ("Tag 6–12 Uhr", 6 * 3600, 12 * 3600),
    ("Tag 12–18 Uhr", 12 * 3600, 18 * 3600),
    ("Tag 18–24 Uhr", 18 * 3600, 24 * 3600),
    ("nach dem Tag", 24 * 3600, i64::MAX),
];

pub fn day_bin(c: &TradeContext) -> Option<usize> {
    DAY_BINS
        .iter()
        .position(|(_, lo, hi)| c.to_day_start >= *lo && c.to_day_start < *hi)
}

pub const PRICE_BINS: [(&str, f64, f64); 6] = [
    ("0–5¢", 0.0, 0.05),
    ("5–15¢", 0.05, 0.15),
    ("15–35¢", 0.15, 0.35),
    ("35–65¢", 0.35, 0.65),
    ("65–85¢", 0.65, 0.85),
    ("85–100¢", 0.85, 1.01),
];

/// Price paid for the side actually bought.
pub fn price_bin(c: &TradeContext) -> Option<usize> {
    let p = if c.direction > 0 {
        c.yes_price
    } else {
        1.0 - c.yes_price
    };
    PRICE_BINS
        .iter()
        .position(|(_, lo, hi)| p >= *lo && p < *hi)
}

pub const Z_BINS: [(&str, f64, f64); 6] = [
    ("< -2 sd", f64::MIN, -2.0),
    ("-2..-1 sd", -2.0, -1.0),
    ("-1..0 sd", -1.0, 0.0),
    ("0..1 sd", 0.0, 1.0),
    ("1..2 sd", 1.0, 2.0),
    ("> 2 sd", 2.0, f64::MAX),
];

/// Bucket position relative to the model forecast, for long-YES trades only.
pub fn z_bin_long(c: &TradeContext) -> Option<usize> {
    if c.direction <= 0 {
        return None;
    }
    let z = c.z?;
    Z_BINS.iter().position(|(_, lo, hi)| z >= *lo && z < *hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(wallet: u32, ts: i64, edge: f64, to_day_start: i64) -> TradeContext {
        TradeContext {
            wallet,
            timestamp: ts,
            usd: 10.0,
            edge,
            yes_price: 0.4,
            direction: 1,
            to_day_start,
            utc_offset: -6 * 3600,
            z: Some(0.5),
        }
    }

    #[test]
    fn ranks_and_bins() {
        let c = vec![
            ctx(1, 3 * 3600 + 120, 0.2, -30 * 3600),
            ctx(1, 3 * 3600 + 400, 0.2, -30 * 3600),
            ctx(2, 15 * 3600, -0.1, 3 * 3600),
            ctx(2, 15 * 3600 + 60, -0.1, 3 * 3600),
        ];
        let top = top_wallets(&c, 2, 1);
        assert_eq!(top[0].0, 1);
        assert!((top[0].1 - 0.2).abs() < 1e-12);

        assert_eq!(utc_hour(&c[0]), Some(3));
        assert_eq!(local_hour(&c[0]), Some(21), "03 UTC is 21 local at UTC-6");
        assert_eq!(minute_slot(&c[1]), Some(1), "6m40s -> slot 5-9 min");
        assert_eq!(day_bin(&c[0]), Some(1), "30 h before the day");
        assert_eq!(day_bin(&c[2]), Some(4), "03:00 on the day");
        assert_eq!(price_bin(&c[0]), Some(3));
        assert_eq!(z_bin_long(&c[0]), Some(3));

        let labels: Vec<String> = (0..24).map(|h| format!("{h:02}")).collect();
        let set = HashSet::from([1u32]);
        let rows = distribution(&c, &set, &labels, utc_hour);
        assert!((rows[3].share_top - 1.0).abs() < 1e-12);
        assert_eq!(rows[3].share_rest, 0.0);
        assert!((rows[15].share_rest - 1.0).abs() < 1e-12);
        assert!((rows[3].edge_top.unwrap() - 0.2).abs() < 1e-12);
        assert_eq!(rows[3].lift, None, "no rest trades in that bin");
    }

    #[test]
    fn short_trades_have_no_z_bin_and_price_of_no_side() {
        let mut c = ctx(1, 0, 0.0, 0);
        c.direction = -1;
        assert_eq!(z_bin_long(&c), None);
        // Short YES at 0.4 = bought NO at 0.6.
        assert_eq!(price_bin(&c), Some(3));
    }
}
