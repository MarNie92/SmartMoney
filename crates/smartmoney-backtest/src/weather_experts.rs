//! Weather experts: can skilled wallets in temperature markets be found in
//! advance, and does copying their trades pay?
//!
//! Wallets are ranked by their edge in period 1 (events before the split
//! date). Their period-2 edge and the return of copying their period-2
//! trades after a delay (at the next real trade of the same bucket, with
//! slippage and taker fee) show whether the skill persists and can be
//! followed. A single wallet can also be profiled over the whole period.

use crate::weather::{BucketTrade, WeatherEvent};
use chrono::NaiveDate;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct ExpertConfig {
    pub split_date: NaiveDate,
    pub p1_min_trades: usize,
    pub p1_min_events: usize,
    pub p2_min_trades: usize,
    pub groups: usize,
    /// Extra row with the best N wallets by period-1 edge.
    pub top_n: usize,
    pub copy_min_usd: f64,
    pub delays_minutes: Vec<u64>,
    pub slippage_bps: f64,
    pub fee_rate: f64,
}

/// Per bucket: event index, event date, final YES payout.
pub struct BucketInfo {
    pub event: usize,
    pub date: NaiveDate,
    pub payout_yes: f64,
}

pub fn bucket_index(events: &[WeatherEvent]) -> HashMap<&str, BucketInfo> {
    events
        .iter()
        .enumerate()
        .flat_map(|(i, e)| {
            e.buckets.iter().map(move |b| {
                (
                    b.condition_id.as_str(),
                    BucketInfo {
                        event: i,
                        date: e.date,
                        payout_yes: b.payout_yes,
                    },
                )
            })
        })
        .collect()
}

/// Edge of a trade at resolution (probability points).
pub fn trade_edge(t: &BucketTrade, payout_yes: f64) -> f64 {
    f64::from(t.direction) * (payout_yes - t.yes_price)
}

/// Return of copying `trades[i]` after `delay` minutes: same direction at
/// the next trade of the bucket at or after the target time.
pub fn copy_roi(
    trades: &[BucketTrade],
    i: usize,
    delay: u64,
    payout_yes: f64,
    slippage_bps: f64,
    fee_rate: f64,
) -> Option<f64> {
    let t = &trades[i];
    let target = t.timestamp + (delay * 60) as i64;
    let next = trades[i + 1..].iter().find(|n| n.timestamp >= target)?;
    let (price, payout) = if t.direction > 0 {
        (next.yes_price, payout_yes)
    } else {
        (1.0 - next.yes_price, 1.0 - payout_yes)
    };
    let p = price * (1.0 + slippage_bps / 10_000.0);
    if !(0.01..0.99).contains(&p) {
        return None;
    }
    Some(payout / (p + fee_rate * p * (1.0 - p)) - 1.0)
}

#[derive(Default)]
struct Acc {
    usd: f64,
    edge_usd: f64,
    trades: usize,
    events: HashSet<usize>,
}

impl Acc {
    fn add(&mut self, t: &BucketTrade, edge: f64, event: usize) {
        self.usd += t.usd;
        self.edge_usd += t.usd * edge;
        self.trades += 1;
        self.events.insert(event);
    }

    fn edge(&self) -> f64 {
        if self.usd > 0.0 {
            self.edge_usd / self.usd
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CopyResult {
    pub delay_minutes: u64,
    pub copies: usize,
    pub events: usize,
    pub roi: Option<f64>,
    pub roi_lo: Option<f64>,
    pub roi_hi: Option<f64>,
}

fn copy_result(delay: u64, rois: &[(usize, f64)]) -> CopyResult {
    if rois.is_empty() {
        return CopyResult {
            delay_minutes: delay,
            copies: 0,
            events: 0,
            roi: None,
            roi_lo: None,
            roi_hi: None,
        };
    }
    let n = rois.len() as f64;
    let mean = rois.iter().map(|(_, r)| r).sum::<f64>() / n;
    let mut clusters: HashMap<usize, f64> = HashMap::new();
    for (e, r) in rois {
        *clusters.entry(*e).or_default() += r - mean;
    }
    let se = clusters.values().map(|s| s * s).sum::<f64>().sqrt() / n;
    CopyResult {
        delay_minutes: delay,
        copies: rois.len(),
        events: clusters.len(),
        roi: Some(mean),
        roi_lo: Some(mean - 1.96 * se),
        roi_hi: Some(mean + 1.96 * se),
    }
}

/// Copy all trades of `wallets` (>= copy_min_usd) that satisfy `period`.
fn copy_trades(
    trades: &HashMap<String, Vec<BucketTrade>>,
    buckets: &HashMap<&str, BucketInfo>,
    wallets: &HashSet<u32>,
    period: impl Fn(NaiveDate) -> bool,
    cfg: &ExpertConfig,
) -> Vec<CopyResult> {
    let mut per_delay: Vec<Vec<(usize, f64)>> = vec![Vec::new(); cfg.delays_minutes.len()];
    for (cid, list) in trades {
        let Some(info) = buckets.get(cid.as_str()) else {
            continue;
        };
        if !period(info.date) {
            continue;
        }
        for (i, t) in list.iter().enumerate() {
            if t.usd < cfg.copy_min_usd || !wallets.contains(&t.wallet) {
                continue;
            }
            for (d, delay) in cfg.delays_minutes.iter().enumerate() {
                if let Some(r) = copy_roi(
                    list,
                    i,
                    *delay,
                    info.payout_yes,
                    cfg.slippage_bps,
                    cfg.fee_rate,
                ) {
                    per_delay[d].push((info.event, r));
                }
            }
        }
    }
    cfg.delays_minutes
        .iter()
        .zip(per_delay)
        .map(|(d, rois)| copy_result(*d, &rois))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpertGroup {
    pub label: String,
    pub wallets: usize,
    pub p1_edge: f64,
    pub p2_edge: f64,
    pub p2_edge_lo: f64,
    pub p2_edge_hi: f64,
    pub p2_trades: usize,
    pub copy: Vec<CopyResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpertReport {
    pub eligible: usize,
    pub spearman: Option<f64>,
    pub groups: Vec<ExpertGroup>,
}

/// Wallets eligible under the config, sorted by period-1 edge (best first), as
/// (wallet, P1 edge, P2 edge), plus the number of P2 trades per wallet.
type Ranked = (Vec<(u32, f64, f64)>, HashMap<u32, usize>);

fn eligible_wallets(
    trades: &HashMap<String, Vec<BucketTrade>>,
    buckets: &HashMap<&str, BucketInfo>,
    cfg: &ExpertConfig,
) -> Ranked {
    let mut p1: HashMap<u32, Acc> = HashMap::new();
    let mut p2: HashMap<u32, Acc> = HashMap::new();
    for (cid, list) in trades {
        let Some(info) = buckets.get(cid.as_str()) else {
            continue;
        };
        let acc = if info.date < cfg.split_date {
            &mut p1
        } else {
            &mut p2
        };
        for t in list {
            acc.entry(t.wallet)
                .or_default()
                .add(t, trade_edge(t, info.payout_yes), info.event);
        }
    }
    let mut eligible: Vec<(u32, f64, f64)> = p1
        .iter()
        .filter(|(_, a)| a.trades >= cfg.p1_min_trades && a.events.len() >= cfg.p1_min_events)
        .filter_map(|(w, a1)| {
            let a2 = p2.get(w)?;
            (a2.trades >= cfg.p2_min_trades).then(|| (*w, a1.edge(), a2.edge()))
        })
        .collect();
    eligible.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let p2_trades = p2.iter().map(|(w, a)| (*w, a.trades)).collect();
    (eligible, p2_trades)
}

/// One robustness configuration: top-N copy result for a split and a
/// slippage assumption.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RobustRow {
    pub split_date: NaiveDate,
    pub top_n: usize,
    pub slippage_bps: f64,
    pub wallets: usize,
    pub p2_edge: f64,
    pub p2_edge_lo: f64,
    pub p2_edge_hi: f64,
    pub copy: Vec<CopyResult>,
}

/// Copy test of the top-N wallets across splits, group sizes and slippage.
pub fn robustness(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    base: &ExpertConfig,
    splits: &[NaiveDate],
    top_ns: &[usize],
    slippages_bps: &[f64],
) -> Vec<RobustRow> {
    let buckets = bucket_index(events);
    let mut rows = Vec::new();
    for split in splits {
        let cfg = ExpertConfig {
            split_date: *split,
            ..base.clone()
        };
        let (eligible, _) = eligible_wallets(trades, &buckets, &cfg);
        for top_n in top_ns {
            let members = &eligible[..(*top_n).min(eligible.len())];
            if members.is_empty() {
                continue;
            }
            let n = members.len() as f64;
            let mean = members.iter().map(|m| m.2).sum::<f64>() / n;
            let var = if members.len() > 1 {
                members.iter().map(|m| (m.2 - mean).powi(2)).sum::<f64>() / (n - 1.0)
            } else {
                0.0
            };
            let se = (var / n).sqrt();
            let wallets: HashSet<u32> = members.iter().map(|m| m.0).collect();
            for slip in slippages_bps {
                let c = ExpertConfig {
                    slippage_bps: *slip,
                    ..cfg.clone()
                };
                let split_date = *split;
                rows.push(RobustRow {
                    split_date,
                    top_n: members.len(),
                    slippage_bps: *slip,
                    wallets: members.len(),
                    p2_edge: mean,
                    p2_edge_lo: mean - 1.96 * se,
                    p2_edge_hi: mean + 1.96 * se,
                    copy: copy_trades(trades, &buckets, &wallets, |d| d >= split_date, &c),
                });
            }
        }
    }
    rows
}

pub fn evaluate(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    cfg: &ExpertConfig,
) -> ExpertReport {
    let buckets = bucket_index(events);
    let (eligible, p2_trades) = eligible_wallets(trades, &buckets, cfg);
    let spearman = crate::smart::spearman(
        &eligible.iter().map(|e| e.1).collect::<Vec<_>>(),
        &eligible.iter().map(|e| e.2).collect::<Vec<_>>(),
    );

    let split = cfg.split_date;
    let group = |label: String, members: &[(u32, f64, f64)]| -> ExpertGroup {
        let n = members.len() as f64;
        let mean = members.iter().map(|m| m.2).sum::<f64>() / n;
        let var = if members.len() > 1 {
            members.iter().map(|m| (m.2 - mean).powi(2)).sum::<f64>() / (n - 1.0)
        } else {
            0.0
        };
        let se = (var / n).sqrt();
        let wallets: HashSet<u32> = members.iter().map(|m| m.0).collect();
        ExpertGroup {
            label,
            wallets: members.len(),
            p1_edge: members.iter().map(|m| m.1).sum::<f64>() / n,
            p2_edge: mean,
            p2_edge_lo: mean - 1.96 * se,
            p2_edge_hi: mean + 1.96 * se,
            p2_trades: members.iter().map(|m| p2_trades[&m.0]).sum(),
            copy: copy_trades(trades, &buckets, &wallets, |d| d >= split, cfg),
        }
    };

    let mut groups = Vec::new();
    if !eligible.is_empty() {
        let top = cfg.top_n.min(eligible.len());
        if top > 0 {
            groups.push(group(format!("Top {top}"), &eligible[..top]));
        }
        let g = cfg.groups.max(1);
        for k in 0..g {
            let from = k * eligible.len() / g;
            let to = (k + 1) * eligible.len() / g;
            if from < to {
                let label = match k {
                    0 => "Bestes 1/5".to_owned(),
                    k if k + 1 == g => "Schlechtestes 1/5".to_owned(),
                    k => format!("{}. Fünftel", k + 1),
                };
                groups.push(group(label, &eligible[from..to]));
            }
        }
    }
    ExpertReport {
        eligible: eligible.len(),
        spearman,
        groups,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalletProfile {
    pub wallet: String,
    pub trades: usize,
    pub events: usize,
    pub usd: f64,
    pub edge: f64,
    pub copy: Vec<CopyResult>,
}

/// Edge and copy results of one wallet over the whole period.
pub fn profile_wallet(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    wallets: &[String],
    address: &str,
    cfg: &ExpertConfig,
) -> Option<WalletProfile> {
    let id = wallets
        .iter()
        .position(|w| w.eq_ignore_ascii_case(address))? as u32;
    let buckets = bucket_index(events);
    let mut acc = Acc::default();
    for (cid, list) in trades {
        let Some(info) = buckets.get(cid.as_str()) else {
            continue;
        };
        for t in list.iter().filter(|t| t.wallet == id) {
            acc.add(t, trade_edge(t, info.payout_yes), info.event);
        }
    }
    let set = HashSet::from([id]);
    Some(WalletProfile {
        wallet: address.to_lowercase(),
        trades: acc.trades,
        events: acc.events.len(),
        usd: acc.usd,
        edge: acc.edge(),
        copy: copy_trades(trades, &buckets, &set, |_| true, cfg),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weather::{parse_bucket, Bucket, Kind, Unit};

    fn t(ts: i64, yes_price: f64, wallet: u32, direction: i8) -> BucketTrade {
        BucketTrade {
            timestamp: ts,
            yes_price,
            usd: 100.0,
            wallet,
            direction,
        }
    }

    #[test]
    fn edges_and_copy_returns_by_direction() {
        // Long YES at 0.40 on a winning bucket: +0.60.
        assert!((trade_edge(&t(0, 0.40, 0, 1), 1.0) - 0.60).abs() < 1e-12);
        // Short YES (bought NO) at yes 0.40 on a losing bucket: +0.40.
        assert!((trade_edge(&t(0, 0.40, 0, -1), 0.0) - 0.40).abs() < 1e-12);

        let list = vec![t(0, 0.40, 1, 1), t(30, 0.45, 2, 1), t(400, 0.50, 3, -1)];
        // Copy immediately: next trade at 0.45, bucket wins.
        let r = copy_roi(&list, 0, 0, 1.0, 0.0, 0.0).unwrap();
        assert!((r - (1.0 / 0.45 - 1.0)).abs() < 1e-12);
        // Copy after 5 min: next trade at t=400, yes 0.50.
        let r = copy_roi(&list, 0, 5, 1.0, 0.0, 0.05).unwrap();
        let cost = 0.50 + 0.05 * 0.5 * 0.5;
        assert!((r - (1.0 / cost - 1.0)).abs() < 1e-12);
        // Short copy buys NO at 1 - yes price; no later trade -> None.
        assert!(copy_roi(&list, 2, 0, 0.0, 0.0, 0.0).is_none());
    }

    fn event(id: &str, date: NaiveDate, winner: usize) -> WeatherEvent {
        let labels = ["49°F or below", "50-51°F", "52°F or higher"];
        WeatherEvent {
            event_id: id.into(),
            kind: Kind::High,
            station: "KORD".into(),
            date,
            unit: Unit::F,
            buckets: labels
                .iter()
                .enumerate()
                .map(|(i, l)| Bucket {
                    condition_id: format!("{id}-{i}"),
                    label: l.to_string(),
                    range: parse_bucket(l).unwrap().0,
                    yes_index: 0,
                    payout_yes: if i == winner { 1.0 } else { 0.0 },
                })
                .collect(),
        }
    }

    #[test]
    fn ranks_wallets_and_tests_persistence() {
        let d0 = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let mut events = Vec::new();
        let mut trades: HashMap<String, Vec<BucketTrade>> = HashMap::new();
        // 20 days; winner is always bucket 1. Wallet 0 buys the winner at
        // 0.40, wallet 1 buys a loser at 0.40; wallet 2 trades after them.
        for k in 0..20 {
            let d = d0 + chrono::Duration::days(k);
            let id = format!("e{k}");
            events.push(event(&id, d, 1));
            let ts = 1_000_000 + k * 86_400;
            trades.insert(
                format!("{id}-1"),
                vec![t(ts, 0.40, 0, 1), t(ts + 600, 0.50, 2, 1)],
            );
            trades.insert(
                format!("{id}-0"),
                vec![t(ts, 0.40, 1, 1), t(ts + 600, 0.30, 2, 1)],
            );
        }
        let cfg = ExpertConfig {
            split_date: d0 + chrono::Duration::days(10),
            p1_min_trades: 5,
            p1_min_events: 5,
            p2_min_trades: 5,
            groups: 2,
            top_n: 1,
            copy_min_usd: 0.0,
            delays_minutes: vec![1],
            slippage_bps: 0.0,
            fee_rate: 0.0,
        };
        let r = evaluate(&events, &trades, &cfg);
        assert_eq!(r.eligible, 3);
        let top = &r.groups[0];
        assert_eq!(top.label, "Top 1");
        assert!((top.p1_edge - 0.60).abs() < 1e-9);
        assert!((top.p2_edge - 0.60).abs() < 1e-9, "skill persists");
        // Copying wallet 0 in period 2: next trade at 0.50, wins -> +100 %.
        let c = &top.copy[0];
        assert_eq!(c.copies, 10);
        assert!((c.roi.unwrap() - 1.0).abs() < 1e-9);

        let wallets = vec!["0xa".to_string(), "0xb".to_string(), "0xc".to_string()];
        let p = profile_wallet(&events, &trades, &wallets, "0xA", &cfg).unwrap();
        assert_eq!(p.trades, 20);
        assert_eq!(p.events, 20);
        assert!((p.edge - 0.60).abs() < 1e-9);
        assert_eq!(p.copy[0].copies, 20);
        assert!(profile_wallet(&events, &trades, &wallets, "0xzz", &cfg).is_none());
    }
    #[test]
    fn robustness_grid_varies_group_size_and_slippage() {
        let d0 = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let mut events = Vec::new();
        let mut trades: HashMap<String, Vec<BucketTrade>> = HashMap::new();
        for k in 0..20 {
            let d = d0 + chrono::Duration::days(k);
            let id = format!("e{k}");
            events.push(event(&id, d, 1));
            let ts = 1_000_000 + k * 86_400;
            trades.insert(
                format!("{id}-1"),
                vec![t(ts, 0.40, 0, 1), t(ts + 600, 0.50, 2, 1)],
            );
            trades.insert(
                format!("{id}-0"),
                vec![t(ts, 0.40, 1, 1), t(ts + 600, 0.30, 2, 1)],
            );
        }
        let base = ExpertConfig {
            split_date: d0,
            p1_min_trades: 5,
            p1_min_events: 5,
            p2_min_trades: 5,
            groups: 1,
            top_n: 1,
            copy_min_usd: 0.0,
            delays_minutes: vec![1],
            slippage_bps: 0.0,
            fee_rate: 0.0,
        };
        let split = d0 + chrono::Duration::days(10);
        let rows = robustness(&events, &trades, &base, &[split], &[1, 2], &[0.0, 100.0]);
        assert_eq!(rows.len(), 4);
        let top1_noslip = &rows[0];
        assert_eq!((top1_noslip.top_n, top1_noslip.slippage_bps), (1, 0.0));
        assert!((top1_noslip.copy[0].roi.unwrap() - 1.0).abs() < 1e-9);
        let top1_slip = &rows[1];
        assert!((top1_slip.copy[0].roi.unwrap() - (1.0 / 0.505 - 1.0)).abs() < 1e-9);
        assert_eq!(rows[2].top_n, 2);
    }
}
