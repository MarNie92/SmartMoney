//! Nowcast backtest: temperature markets on the event day itself.
//!
//! At a decision time during the day (local `decision_hour`:58, just after
//! the routine METAR at :51-:53), the resolution value is
//!   daily max = max(observed max so far, max of the remaining hours)
//! (min for lowest-temperature markets). The observed part is a hard bound.
//! The remaining part is the mean of several models' hourly forecasts
//! (issued at least 24 h earlier) for the rest of the day, corrected by the
//! station's typical error of that quantity at that decision hour, learned
//! from days resolved before the decision (no lookahead).

use crate::weather::{norm_cdf, BucketTrade, Kind, Observation, Range, Unit, WeatherEvent};
use crate::weather_v2::StationData;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct NowcastConfig {
    /// Local hour of the decision (decision at hour:58).
    pub decision_hour: u32,
    pub history_days: i64,
    pub min_history: usize,
    pub sd_inflation: f64,
    pub sd_floor_f: f64,
    pub sd_floor_c: f64,
    pub min_price_usd: f64,
    /// Maximum age of the market price at the decision (seconds).
    pub max_price_age: i64,
}

fn decision_local(day: NaiveDate, hour: u32) -> NaiveDateTime {
    day.and_time(NaiveTime::from_hms_opt(hour, 58, 0).unwrap())
}

/// UTC timestamp of a local time (earliest if ambiguous).
pub fn to_utc(tz: &Tz, local: NaiveDateTime) -> Option<i64> {
    tz.from_local_datetime(&local)
        .earliest()
        .map(|t| t.timestamp())
}

fn pick(values: impl Iterator<Item = f64>, kind: Kind) -> Option<f64> {
    values.fold(None, |acc: Option<f64>, v| {
        Some(match (acc, kind) {
            (None, _) => v,
            (Some(a), Kind::High) => a.max(v),
            (Some(a), Kind::Low) => a.min(v),
        })
    })
}

/// Observed extreme of the local day up to and including `until`.
pub fn observed_so_far(
    obs: &[(NaiveDateTime, f64)],
    day: NaiveDate,
    until: NaiveDateTime,
    kind: Kind,
) -> Option<f64> {
    pick(
        obs.iter()
            .filter(|(t, _)| t.date() == day && *t <= until)
            .map(|(_, v)| *v),
        kind,
    )
}

/// Observed extreme of the local day strictly after `after`.
pub fn observed_after(
    obs: &[(NaiveDateTime, f64)],
    day: NaiveDate,
    after: NaiveDateTime,
    kind: Kind,
) -> Option<f64> {
    pick(
        obs.iter()
            .filter(|(t, _)| t.date() == day && *t > after)
            .map(|(_, v)| *v),
        kind,
    )
}

/// Forecast extreme of the local hours after `hour` (hour+1 .. 23).
pub fn forecast_after(
    fc: &HashMap<NaiveDateTime, f64>,
    day: NaiveDate,
    hour: u32,
    kind: Kind,
) -> Option<f64> {
    pick(
        (hour + 1..24).filter_map(|h| {
            fc.get(&day.and_time(NaiveTime::from_hms_opt(h, 0, 0).unwrap()))
                .copied()
        }),
        kind,
    )
}

/// P(round(resolution value) in range) where the resolution value is the
/// extreme of a known bound and a Normal(mu, sd) remainder.
pub fn bucket_prob(range: Range, kind: Kind, bound: f64, rest: Option<(f64, f64)>) -> f64 {
    // CDF of the resolution value D.
    let cdf = |x: f64| -> f64 {
        match (kind, rest) {
            (Kind::High, None) => f64::from(u8::from(x >= bound)),
            (Kind::Low, None) => f64::from(u8::from(x >= bound)),
            // D = max(bound, R): P(D <= x) = 1[x >= bound] * P(R <= x).
            (Kind::High, Some((mu, sd))) => {
                if x < bound {
                    0.0
                } else {
                    norm_cdf((x - mu) / sd)
                }
            }
            // D = min(bound, R): P(D <= x) = 1 - 1[bound > x] * P(R > x).
            (Kind::Low, Some((mu, sd))) => {
                if bound <= x {
                    1.0
                } else {
                    norm_cdf((x - mu) / sd)
                }
            }
        }
    };
    let upper = range.hi.map_or(1.0, |h| cdf(h as f64 + 0.5));
    // Values below lo - 0.5 round below the bucket.
    let lower = range.lo.map_or(0.0, |l| cdf(l as f64 - 0.5 - 1e-9));
    (upper - lower).max(0.0)
}

/// Remaining-hours distribution (mu, sd) for one event at its decision hour.
pub fn remainder(
    st: &StationData,
    kind: Kind,
    unit: Unit,
    day: NaiveDate,
    cfg: &NowcastConfig,
) -> Option<Option<(f64, f64)>> {
    if cfg.decision_hour >= 23 {
        return Some(None);
    }
    let f_today = forecast_after(&st.hourly_fc, day, cfg.decision_hour, kind)?;
    let errors: Vec<f64> = (2..=cfg.history_days)
        .filter_map(|k| {
            let d = day - chrono::Duration::days(k);
            let f = forecast_after(&st.hourly_fc, d, cfg.decision_hour, kind)?;
            let a = observed_after(
                &st.hourly_obs,
                d,
                decision_local(d, cfg.decision_hour),
                kind,
            )?;
            Some(a - f)
        })
        .collect();
    if errors.len() < cfg.min_history {
        return None;
    }
    let n = errors.len() as f64;
    let bias = errors.iter().sum::<f64>() / n;
    let var = errors.iter().map(|e| (e - bias).powi(2)).sum::<f64>() / (n - 1.0);
    let floor = match unit {
        Unit::F => cfg.sd_floor_f,
        Unit::C => cfg.sd_floor_c,
    };
    Some(Some((
        f_today + bias,
        (var.sqrt() * cfg.sd_inflation).max(floor),
    )))
}

/// One observation per bucket with a fresh market price at the decision.
pub fn observations(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    stations: &HashMap<String, StationData>,
    cfg: &NowcastConfig,
) -> Vec<Observation> {
    let mut out = Vec::new();
    for e in events {
        let Some(st) = stations.get(&e.station) else {
            continue;
        };
        let Ok(tz) = st.timezone.parse::<Tz>() else {
            continue;
        };
        let local = decision_local(e.date, cfg.decision_hour);
        let Some(decision) = to_utc(&tz, local) else {
            continue;
        };
        let Some(bound) = observed_so_far(&st.hourly_obs, e.date, local, e.kind) else {
            continue;
        };
        let Some(rest) = remainder(st, e.kind, e.unit, e.date, cfg) else {
            continue;
        };
        let probs: Vec<f64> = e
            .buckets
            .iter()
            .map(|b| bucket_prob(b.range, e.kind, bound, rest))
            .collect();
        let total: f64 = probs.iter().sum();
        if total <= 0.0 {
            continue;
        }
        for (b, p) in e.buckets.iter().zip(probs) {
            let Some(market) = trades.get(&b.condition_id).and_then(|ts| {
                ts.iter()
                    .filter(|t| {
                        t.timestamp <= decision
                            && t.timestamp >= decision - cfg.max_price_age
                            && t.usd >= cfg.min_price_usd
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
                model: p / total,
                market,
                outcome: b.payout_yes,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(day: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap())
    }

    #[test]
    fn observed_extremes_respect_the_decision_time() {
        let obs = vec![
            (t(1, 9, 51), 60.0),
            (t(1, 12, 51), 66.0),
            (t(1, 15, 51), 71.0),
            (t(2, 1, 51), 99.0),
        ];
        let day = t(1, 0, 0).date();
        assert_eq!(
            observed_so_far(&obs, day, t(1, 12, 58), Kind::High),
            Some(66.0)
        );
        assert_eq!(
            observed_so_far(&obs, day, t(1, 12, 58), Kind::Low),
            Some(60.0)
        );
        assert_eq!(
            observed_after(&obs, day, t(1, 12, 58), Kind::High),
            Some(71.0),
            "next day excluded"
        );
    }

    #[test]
    fn bound_rules_out_lower_buckets_for_highs() {
        let below = Range {
            lo: Some(60),
            hi: Some(61),
        };
        let at = Range {
            lo: Some(66),
            hi: Some(67),
        };
        let above = Range {
            lo: Some(70),
            hi: None,
        };
        // Already 66 observed; rest of day expected 64 +- 1.
        assert_eq!(bucket_prob(below, Kind::High, 66.0, Some((64.0, 1.0))), 0.0);
        let p_at = bucket_prob(at, Kind::High, 66.0, Some((64.0, 1.0)));
        assert!(p_at > 0.95, "bound dominates: {p_at}");
        assert!(bucket_prob(above, Kind::High, 66.0, Some((64.0, 1.0))) < 0.01);
        // No remaining hours: exactly the bound.
        assert_eq!(bucket_prob(at, Kind::High, 66.0, None), 1.0);
    }

    #[test]
    fn bound_rules_out_higher_buckets_for_lows() {
        let above = Range {
            lo: Some(50),
            hi: Some(51),
        };
        let at = Range {
            lo: Some(44),
            hi: Some(45),
        };
        // Already 44 observed as minimum; evening expected 48 +- 1.
        assert_eq!(bucket_prob(above, Kind::Low, 44.0, Some((48.0, 1.0))), 0.0);
        assert!(bucket_prob(at, Kind::Low, 44.0, Some((48.0, 1.0))) > 0.99);
        // Colder evening possible: mass moves below the bound.
        let lower = Range {
            lo: None,
            hi: Some(43),
        };
        assert!(bucket_prob(lower, Kind::Low, 44.0, Some((42.0, 1.0))) > 0.7);
    }

    fn station() -> StationData {
        let mut st = StationData {
            timezone: "America/Chicago".into(),
            ..Default::default()
        };
        // 30 days: forecast afternoon 70, observed afternoon 72 (bias +2).
        for d in 1..=30 {
            for h in 0..24 {
                let f = if h >= 13 { 70.0 } else { 60.0 };
                st.hourly_fc.insert(t(d, h, 0), f);
                let o = if h >= 13 { 72.0 } else { 60.0 };
                st.hourly_obs.push((t(d, h, 51), o));
            }
        }
        st.hourly_obs.sort_by_key(|x| x.0);
        st
    }

    fn cfg() -> NowcastConfig {
        NowcastConfig {
            decision_hour: 12,
            history_days: 60,
            min_history: 10,
            sd_inflation: 1.0,
            sd_floor_f: 0.5,
            sd_floor_c: 0.3,
            min_price_usd: 0.0,
            max_price_age: 1800,
        }
    }

    #[test]
    fn learns_remaining_hours_bias_from_past_days_only() {
        let mut st = station();
        let day = t(30, 0, 0).date();
        let (mu, sd) = remainder(&st, Kind::High, Unit::F, day, &cfg())
            .unwrap()
            .unwrap();
        assert!((mu - 72.0).abs() < 1e-9, "70 + learned bias 2: {mu}");
        assert_eq!(sd, 0.5, "no spread -> floor");
        // Changing today's and yesterday's afternoon observations must not
        // change the learned remainder.
        for (time, v) in st.hourly_obs.iter_mut() {
            if time.date() >= t(29, 0, 0).date() && time.time().hour_after(12) {
                *v = 99.0;
            }
        }
        let again = remainder(&st, Kind::High, Unit::F, day, &cfg())
            .unwrap()
            .unwrap();
        assert_eq!(again, (mu, sd));
    }

    trait HourAfter {
        fn hour_after(&self, h: u32) -> bool;
    }
    impl HourAfter for NaiveTime {
        fn hour_after(&self, h: u32) -> bool {
            use chrono::Timelike;
            self.hour() > h
        }
    }

    #[test]
    fn decision_time_converts_with_daylight_saving() {
        let tz: Tz = "America/Chicago".parse().unwrap();
        // 2026-09-01 is CDT (UTC-5): 12:58 local = 17:58 UTC.
        let summer = to_utc(&tz, t(1, 12, 58)).unwrap();
        assert_eq!(summer % 86_400, 17 * 3600 + 58 * 60);
        // 2026-11-10 is CST (UTC-6): 12:58 local = 18:58 UTC.
        let winter_local = NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(12, 58, 0).unwrap());
        let winter = to_utc(&tz, winter_local).unwrap();
        assert_eq!(winter % 86_400, 18 * 3600 + 58 * 60);
    }
}
