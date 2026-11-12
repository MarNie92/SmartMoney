//! Weather model v2: multi-model forecasts calibrated on station observations.
//!
//! For every station, kind (daily high / low) and lead time:
//! - each of several global weather models (Open-Meteo previous-runs
//!   archive) is bias-corrected with its mean error over the last
//!   `history_days` resolved days, measured against the station's own METAR
//!   observations (the data the markets resolve on);
//! - the corrected forecasts are combined with weights proportional to the
//!   inverse of each model's error variance;
//! - the spread is the RMSE of that combination over the same days, times an
//!   inflation factor (the fit is in-sample), with a floor.
//!
//! Only days at least two days before the target date are used, so every
//! observation was known at the decision time (no lookahead).

use crate::weather::{
    bucket_prob, compare, decision_ts, strategy, BucketTrade, Kind, Observation, ScoreComparison,
    StrategyResult, Unit, WeatherEvent,
};
use chrono::NaiveDate;
use serde::Serialize;
use std::collections::HashMap;

/// Global models used (Open-Meteo identifiers).
pub const MODELS: [&str; 7] = [
    "ecmwf_ifs025",
    "gfs_seamless",
    "icon_seamless",
    "gem_seamless",
    "jma_seamless",
    "ukmo_seamless",
    "meteofrance_seamless",
];

/// Daily extremes as (max, min).
pub type Extremes = (f64, f64);

#[derive(Debug, Clone, Default)]
pub struct StationData {
    /// IANA time zone of the station (from the forecast response).
    pub timezone: String,
    pub utc_offset: i64,
    /// (local date, lead) -> per model (index into `MODELS`) daily extremes.
    pub forecasts: HashMap<(NaiveDate, u8), Vec<Option<Extremes>>>,
    /// Local date -> observed daily extremes (routine METAR reports).
    pub obs: HashMap<NaiveDate, Extremes>,
}

fn extreme(e: Extremes, kind: Kind) -> f64 {
    match kind {
        Kind::High => e.0,
        Kind::Low => e.1,
    }
}

fn group_extremes<'a>(
    rows: impl Iterator<Item = (NaiveDate, f64)> + 'a,
    min_count: usize,
) -> HashMap<NaiveDate, Extremes> {
    let mut per_day: HashMap<NaiveDate, (f64, f64, usize)> = HashMap::new();
    for (d, v) in rows {
        let e = per_day.entry(d).or_insert((f64::MIN, f64::MAX, 0));
        e.0 = e.0.max(v);
        e.1 = e.1.min(v);
        e.2 += 1;
    }
    per_day
        .into_iter()
        .filter(|(_, (_, _, n))| *n >= min_count)
        .map(|(d, (max, min, _))| (d, (max, min)))
        .collect()
}

/// Parse an Open-Meteo previous-runs response with `models=` (hourly keys
/// `temperature_2m_previous_day{lead}_{model}`, local times).
pub fn parse_multi_forecast(raw: &serde_json::Value) -> StationData {
    let mut data = StationData {
        timezone: raw
            .get("timezone")
            .and_then(|v| v.as_str())
            .unwrap_or("UTC")
            .to_owned(),
        utc_offset: raw
            .get("utc_offset_seconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        ..Default::default()
    };
    let Some(hourly) = raw.get("hourly") else {
        return data;
    };
    let dates: Vec<Option<NaiveDate>> = hourly
        .get("time")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|t| {
                    t.as_str()
                        .and_then(|s| s.get(..10))
                        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                })
                .collect()
        })
        .unwrap_or_default();
    for lead in [1u8, 2] {
        for (m, model) in MODELS.iter().enumerate() {
            let key = format!("temperature_2m_previous_day{lead}_{model}");
            let Some(values) = hourly.get(&key).and_then(|v| v.as_array()) else {
                continue;
            };
            let rows = dates
                .iter()
                .zip(values)
                .filter_map(|(d, v)| Some(((*d)?, v.as_f64()?)));
            for (date, ext) in group_extremes(rows, 20) {
                let slot = data
                    .forecasts
                    .entry((date, lead))
                    .or_insert_with(|| vec![None; MODELS.len()]);
                slot[m] = Some(ext);
            }
        }
    }
    data
}

/// Parse an IEM ASOS CSV (`station,valid,value`, local times) into daily
/// extremes per local date; days with fewer than `min_count` reports are
/// dropped.
pub fn parse_iem_csv(text: &str, min_count: usize) -> HashMap<NaiveDate, Extremes> {
    let rows = text.lines().skip(1).filter_map(|line| {
        let mut parts = line.split(',');
        let _station = parts.next()?;
        let valid = parts.next()?;
        let value = parts.next()?.trim().parse::<f64>().ok()?;
        let date = NaiveDate::parse_from_str(valid.get(..10)?, "%Y-%m-%d").ok()?;
        Some((date, value))
    });
    group_extremes(rows, min_count)
}

#[derive(Debug, Clone)]
pub struct V2Config {
    pub lead: u8,
    pub history_days: i64,
    pub min_history: usize,
    pub sd_inflation: f64,
    pub sd_floor_f: f64,
    pub sd_floor_c: f64,
    pub min_price_usd: f64,
    pub max_price_age: Option<i64>,
}

/// Forecast distribution (mean, sd) for one station, kind and date.
pub fn predict(
    st: &StationData,
    kind: Kind,
    unit: Unit,
    date: NaiveDate,
    cfg: &V2Config,
) -> Option<(f64, f64)> {
    let target = st.forecasts.get(&(date, cfg.lead))?;
    let from = date - chrono::Duration::days(cfg.history_days);
    let to = date - chrono::Duration::days(2);
    let mut days: Vec<NaiveDate> = st
        .obs
        .keys()
        .copied()
        .filter(|d| *d >= from && *d <= to)
        .collect();
    days.sort();

    // Per model: bias and error variance over the training days.
    let mut params: Vec<Option<(f64, f64)>> = vec![None; MODELS.len()];
    for (m, slot) in params.iter_mut().enumerate() {
        if target[m].is_none() {
            continue;
        }
        let errors: Vec<f64> = days
            .iter()
            .filter_map(|d| {
                let f = st.forecasts.get(&(*d, cfg.lead))?.get(m).copied()??;
                Some(extreme(st.obs[d], kind) - extreme(f, kind))
            })
            .collect();
        if errors.len() < cfg.min_history {
            continue;
        }
        let n = errors.len() as f64;
        let bias = errors.iter().sum::<f64>() / n;
        let var = errors.iter().map(|e| (e - bias).powi(2)).sum::<f64>() / (n - 1.0);
        *slot = Some((bias, var.max(0.05)));
    }
    let combine = |forecasts: &[Option<Extremes>]| -> Option<f64> {
        let (mut num, mut den) = (0.0, 0.0);
        for (m, p) in params.iter().enumerate() {
            let (Some((bias, var)), Some(Some(f))) = (p, forecasts.get(m)) else {
                continue;
            };
            let w = 1.0 / var;
            num += w * (extreme(*f, kind) + bias);
            den += w;
        }
        (den > 0.0).then(|| num / den)
    };
    let mu = combine(target)?;

    // Spread: RMSE of the combined forecast over the training days.
    let residuals: Vec<f64> = days
        .iter()
        .filter_map(|d| {
            let fc = st.forecasts.get(&(*d, cfg.lead))?;
            Some(extreme(st.obs[d], kind) - combine(fc)?)
        })
        .collect();
    if residuals.len() < cfg.min_history {
        return None;
    }
    let rmse = (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len() as f64).sqrt();
    let floor = match unit {
        Unit::F => cfg.sd_floor_f,
        Unit::C => cfg.sd_floor_c,
    };
    Some((mu, (rmse * cfg.sd_inflation).max(floor)))
}

/// Model v2 observations (one per bucket with a market price).
pub fn observations(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    stations: &HashMap<String, StationData>,
    cfg: &V2Config,
) -> Vec<Observation> {
    let mut out = Vec::new();
    for e in events {
        let Some(st) = stations.get(&e.station) else {
            continue;
        };
        let Some((mu, sd)) = predict(st, e.kind, e.unit, e.date, cfg) else {
            continue;
        };
        let probs: Vec<f64> = e
            .buckets
            .iter()
            .map(|b| bucket_prob(b.range, mu, sd))
            .collect();
        let total: f64 = probs.iter().sum();
        let decision = decision_ts(e.date, cfg.lead, st.utc_offset);
        for (b, p) in e.buckets.iter().zip(probs) {
            let Some(market) = trades.get(&b.condition_id).and_then(|ts| {
                ts.iter()
                    .filter(|t| {
                        t.timestamp <= decision
                            && t.usd >= cfg.min_price_usd
                            && cfg
                                .max_price_age
                                .is_none_or(|age| t.timestamp >= decision - age)
                    })
                    .max_by_key(|t| t.timestamp)
                    .map(|t| t.yes_price)
            }) else {
                continue;
            };
            out.push(Observation {
                event_id: e.event_id.clone(),
                kind: e.kind,
                station: e.station.clone(),
                date: e.date,
                label: b.label.clone(),
                model: if total > 0.0 { p / total } else { p },
                market,
                outcome: b.payout_yes,
            });
        }
    }
    out
}

/// Pre-registered success criterion (see ROADMAP.md, Phase 1d).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CriterionReport {
    pub overall: Option<ScoreComparison>,
    pub half1: Option<ScoreComparison>,
    pub half2: Option<ScoreComparison>,
    pub split_date: Option<NaiveDate>,
    /// Threshold with the best taker return in half 1 (>= `min_bets` bets).
    pub chosen_threshold: Option<f64>,
    pub half1_strategies: Vec<StrategyResult>,
    /// Out-of-sample result in half 2 at the chosen threshold.
    pub half2_result: Option<StrategyResult>,
    /// 1: more accurate than the market (CI > 0) overall.
    pub more_accurate: bool,
    /// 2: only fresh prices were used.
    pub fresh_prices: bool,
    /// 3: more accurate in both halves (CI > 0 each).
    pub both_halves: bool,
    /// 4: positive taker return after fees in half 2 at the half-1 threshold.
    pub out_of_sample_profit: bool,
}

impl CriterionReport {
    pub fn passed(&self) -> bool {
        self.more_accurate && self.fresh_prices && self.both_halves && self.out_of_sample_profit
    }
}

pub fn criterion(
    obs: &[Observation],
    fresh_prices: bool,
    thresholds: &[f64],
    min_bets: usize,
    fee_rate: f64,
    slippage_bps: f64,
) -> CriterionReport {
    let mut dates: Vec<NaiveDate> = obs.iter().map(|o| o.date).collect();
    dates.sort();
    let split = dates.get(dates.len() / 2).copied();
    let (h1, h2): (Vec<Observation>, Vec<Observation>) = match split {
        Some(s) => obs.iter().cloned().partition(|o| o.date < s),
        None => (Vec::new(), Vec::new()),
    };
    let overall = compare(obs);
    let half1 = compare(&h1);
    let half2 = compare(&h2);
    let half1_strategies: Vec<StrategyResult> = thresholds
        .iter()
        .filter_map(|t| strategy(&h1, *t, fee_rate, slippage_bps, 10.0))
        .collect();
    let chosen = half1_strategies
        .iter()
        .filter(|s| s.bets >= min_bets)
        .max_by(|a, b| a.roi_taker.total_cmp(&b.roi_taker))
        .map(|s| s.threshold);
    let half2_result = chosen.and_then(|t| strategy(&h2, t, fee_rate, slippage_bps, 10.0));
    let ci_positive =
        |c: &Option<ScoreComparison>| c.as_ref().is_some_and(|c| c.advantage_lo > 0.0);
    CriterionReport {
        more_accurate: ci_positive(&overall),
        fresh_prices,
        both_halves: ci_positive(&half1) && ci_positive(&half2),
        out_of_sample_profit: half2_result.as_ref().is_some_and(|r| r.roi_taker > 0.0),
        overall,
        half1,
        half2,
        split_date: split,
        chosen_threshold: chosen,
        half1_strategies,
        half2_result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, day).unwrap()
    }

    #[test]
    fn parses_multi_model_forecasts() {
        let times: Vec<String> = (0..24).map(|h| format!("2026-09-01T{h:02}:00")).collect();
        let a: Vec<f64> = (0..24).map(|h| 60.0 + h as f64).collect();
        let raw = json!({
            "timezone": "America/Chicago",
            "utc_offset_seconds": -18000,
            "hourly": {
                "time": times,
                "temperature_2m_previous_day1_ecmwf_ifs025": a,
                "temperature_2m_previous_day2_gfs_seamless": a,
            }
        });
        let st = parse_multi_forecast(&raw);
        assert_eq!(st.timezone, "America/Chicago");
        let f1 = &st.forecasts[&(d(1), 1)];
        assert_eq!(f1[0], Some((83.0, 60.0)));
        assert_eq!(f1[1], None, "gfs only has lead 2");
        assert_eq!(st.forecasts[&(d(1), 2)][1], Some((83.0, 60.0)));
    }

    #[test]
    fn parses_iem_csv() {
        let mut text = String::from("station,valid,tmpf\n");
        for h in 0..24 {
            text.push_str(&format!("ORD,2026-09-01 {h:02}:51,{}\n", 50 + h));
        }
        text.push_str("ORD,2026-09-02 00:51,M\n");
        text.push_str("ORD,2026-09-02 01:51,70\n");
        let obs = parse_iem_csv(&text, 12);
        assert_eq!(obs.get(&d(1)), Some(&(73.0, 50.0)));
        assert!(!obs.contains_key(&d(2)), "too few reports");
    }

    fn station(days: u32, model_bias: [f64; 2]) -> StationData {
        // Truth: 70 every day. Model 0 is 2 too cold (+noise), model 1 is
        // 3 too warm with more noise.
        let mut st = StationData::default();
        for k in 1..=days {
            let noise0 = if k % 2 == 0 { 0.5 } else { -0.5 };
            let noise1 = if k % 2 == 0 { 2.0 } else { -2.0 };
            st.obs.insert(d(k), (70.0, 50.0));
            let mut v = vec![None; MODELS.len()];
            v[0] = Some((70.0 - model_bias[0] + noise0, 50.0));
            v[1] = Some((70.0 + model_bias[1] + noise1, 50.0));
            st.forecasts.insert((d(k), 1), v);
        }
        st
    }

    fn cfg() -> V2Config {
        V2Config {
            lead: 1,
            history_days: 60,
            min_history: 10,
            sd_inflation: 1.0,
            sd_floor_f: 0.1,
            sd_floor_c: 0.1,
            min_price_usd: 0.0,
            max_price_age: None,
        }
    }

    #[test]
    fn corrects_bias_and_weights_accurate_models() {
        let st = station(25, [2.0, 3.0]);
        let (mu, sd) = predict(&st, Kind::High, Unit::F, d(25), &cfg()).unwrap();
        assert!((mu - 70.0).abs() < 0.6, "bias-corrected mean {mu}");
        assert!(sd < 1.0, "accurate model dominates: sd {sd}");
        // Not enough history -> no prediction.
        assert!(predict(&st, Kind::High, Unit::F, d(8), &cfg()).is_none());
    }

    #[test]
    fn never_learns_from_the_target_day_or_the_day_before() {
        let mut st = station(25, [2.0, 3.0]);
        // Corrupt the observations of the target day and the day before:
        // the prediction must not change.
        let before = predict(&st, Kind::High, Unit::F, d(25), &cfg()).unwrap();
        st.obs.insert(d(25), (99.0, 50.0));
        st.obs.insert(d(24), (99.0, 50.0));
        let after = predict(&st, Kind::High, Unit::F, d(25), &cfg()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn criterion_requires_all_checks() {
        let mk = |day: u32, model: f64, market: f64, outcome: f64| Observation {
            event_id: format!("e{day}"),
            kind: Kind::High,
            station: "X".into(),
            date: d(day),
            label: "b".into(),
            model,
            market,
            outcome,
        };
        // Model sharper than market every day, both halves.
        let obs: Vec<Observation> = (1..=20)
            .flat_map(|k| [mk(k, 0.9, 0.5, 1.0), mk(k, 0.1, 0.5, 0.0)])
            .collect();
        let r = criterion(&obs, true, &[0.1, 0.2], 1, 0.05, 50.0);
        assert!(r.more_accurate && r.both_halves && r.out_of_sample_profit);
        assert!(r.passed());
        assert_eq!(r.split_date, Some(d(11)));

        let stale = criterion(&obs, false, &[0.1], 1, 0.05, 50.0);
        assert!(!stale.passed(), "stale prices never pass");
    }
}
