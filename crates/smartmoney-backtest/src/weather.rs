//! Weather backtest: daily highest / lowest temperature markets.
//!
//! Polymarket lists one event per city and day ("Highest temperature in
//! Chicago on November 9?") with one Yes/No market per temperature bucket
//! ("58-59°F", "17°C", "49°F or below"). The winning bucket is the one
//! containing the extreme temperature reported by the airport station.
//!
//! Model: a weather forecast issued well before the decision time
//! (Open-Meteo previous-runs archive, lead 24 h or 48 h) plus an error
//! distribution learned per station, kind and lead from days that were
//! already resolved at the decision time (no lookahead). The bucket
//! probability is P(round(T) in bucket) for T ~ Normal(forecast + bias, sd).
//!
//! The model is compared with the market price at the decision time (last
//! trade of at least `min_price_usd` before it).

use chrono::NaiveDate;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

const DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Kind {
    High,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Unit {
    F,
    C,
}

/// Inclusive integer range of a bucket; `None` = open end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub lo: Option<i32>,
    pub hi: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub condition_id: String,
    pub label: String,
    pub range: Range,
    /// Index of the "Yes" outcome in the market.
    pub yes_index: usize,
    /// Final payout of "Yes" (1 = this bucket won).
    pub payout_yes: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeatherEvent {
    pub event_id: String,
    pub kind: Kind,
    pub station: String,
    pub date: NaiveDate,
    pub unit: Unit,
    pub buckets: Vec<Bucket>,
}

/// One trade of a bucket market: (condition_id, timestamp, YES price, usd).
#[derive(Debug, Clone, PartialEq)]
pub struct BucketTrade {
    pub condition_id: String,
    pub timestamp: i64,
    pub yes_price: f64,
    pub usd: f64,
}

pub fn parse_kind(title: &str) -> Option<Kind> {
    let t = title.to_lowercase();
    if t.starts_with("highest temperature") {
        Some(Kind::High)
    } else if t.starts_with("lowest temperature") {
        Some(Kind::Low)
    } else {
        None
    }
}

/// ICAO station from the resolution source (`ids=KORD`) or the description
/// ("(KORD)").
pub fn parse_station(description: &str, resolution_source: &str) -> Option<String> {
    let is_icao = |s: &str| s.len() == 4 && s.chars().all(|c| c.is_ascii_alphanumeric());
    // aviationweather.gov "...?ids=KORD", weather.gov "...?site=zhcc".
    for key in ["ids=", "site="] {
        if let Some(i) = resolution_source.find(key) {
            let id: String = resolution_source[i + key.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if is_icao(&id) {
                return Some(id.to_uppercase());
            }
        }
    }
    // wunderground.com ".../history/daily/cn/jinan/ZSJN" (station last).
    if resolution_source.contains("wunderground.com") {
        let last = resolution_source
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if is_icao(last)
            && last
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Some(last.to_owned());
        }
    }
    description
        .split('(')
        .skip(1)
        .filter_map(|part| part.split(')').next())
        .find(|s| {
            is_icao(s)
                && s.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
        .map(str::to_owned)
}

/// Date from a slug like "highest-temperature-in-chicago-on-november-9-2026".
pub fn parse_date(slug: &str) -> Option<NaiveDate> {
    let parts: Vec<&str> = slug.rsplitn(4, '-').collect();
    if parts.len() < 3 {
        return None;
    }
    let (year, day, month) = (parts[0], parts[1], parts[2]);
    NaiveDate::parse_from_str(&format!("{month} {day} {year}"), "%B %d %Y").ok()
}

/// "49°F or below", "50-51°F", "68°F or higher", "17°C", "-2°C or below".
pub fn parse_bucket(label: &str) -> Option<(Range, Unit)> {
    let l = label.trim().replace('−', "-");
    let unit = if l.contains("°F") {
        Unit::F
    } else if l.contains("°C") {
        Unit::C
    } else {
        return None;
    };
    let lower = l.to_lowercase();
    let body = l.split('°').next()?.trim();
    let num = |s: &str| s.trim().parse::<i32>().ok();
    let range = if lower.contains("or below") || lower.contains("or lower") {
        Range {
            lo: None,
            hi: Some(num(body)?),
        }
    } else if lower.contains("or higher") || lower.contains("or above") {
        Range {
            lo: Some(num(body)?),
            hi: None,
        }
    } else {
        // Split "50-51" at a '-' that is not a leading sign.
        match body[1..].find('-').map(|i| i + 1) {
            Some(i) => Range {
                lo: Some(num(&body[..i])?),
                hi: Some(num(&body[i + 1..])?),
            },
            None => {
                let v = num(body)?;
                Range {
                    lo: Some(v),
                    hi: Some(v),
                }
            }
        }
    };
    Some((range, unit))
}

/// Standard normal CDF (Abramowitz–Stegun 7.1.26, |error| < 1.5e-7).
pub fn norm_cdf(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// P(round(T) in range) for T ~ Normal(mu, sd).
pub fn bucket_prob(range: Range, mu: f64, sd: f64) -> f64 {
    let upper = range
        .hi
        .map_or(1.0, |h| norm_cdf((h as f64 + 0.5 - mu) / sd));
    let lower = range
        .lo
        .map_or(0.0, |l| norm_cdf((l as f64 - 0.5 - mu) / sd));
    (upper - lower).max(0.0)
}

/// Representative value of the winning bucket (used to learn forecast
/// errors). Open ends use one bucket width beyond the bound.
pub fn bucket_value(range: Range, unit: Unit) -> Option<f64> {
    let width = match unit {
        Unit::F => 2.0,
        Unit::C => 1.0,
    };
    match (range.lo, range.hi) {
        (Some(l), Some(h)) => Some((l + h) as f64 / 2.0),
        (None, Some(h)) => Some(h as f64 - width / 2.0),
        (Some(l), None) => Some(l as f64 + width / 2.0),
        (None, None) => None,
    }
}

/// Forecast of the daily extreme at one lead (days before the valid hour).
#[derive(Debug, Clone, Default)]
pub struct StationForecasts {
    /// (date, lead) -> (daily max, daily min) in the station's unit.
    pub daily: HashMap<(NaiveDate, u8), (f64, f64)>,
    /// UTC offset of the station (seconds), from the forecast response.
    pub utc_offset: i64,
}

impl StationForecasts {
    pub fn value(&self, date: NaiveDate, lead: u8, kind: Kind) -> Option<f64> {
        let (max, min) = *self.daily.get(&(date, lead))?;
        Some(match kind {
            Kind::High => max,
            Kind::Low => min,
        })
    }
}

/// Decision time for a lead: lead 2 at 06:00 local on the day before,
/// lead 1 at 00:00 local on the day itself. Both are after the issue time
/// of every hourly forecast of the day at that lead.
pub fn decision_ts(date: NaiveDate, lead: u8, utc_offset: i64) -> i64 {
    let midnight_local = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - utc_offset;
    match lead {
        1 => midnight_local,
        _ => midnight_local - DAY + 6 * 3600,
    }
}

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub lead: u8,
    /// Days of resolved history used to learn bias and spread.
    pub history_days: i64,
    pub min_history: usize,
    /// Minimum spread (station unit), against overconfidence.
    pub sd_floor_f: f64,
    pub sd_floor_c: f64,
    /// Only trades of at least this size set the market price.
    pub min_price_usd: f64,
    /// Ignore prices older than this before the decision (seconds);
    /// `None` = any age.
    pub max_price_age: Option<i64>,
}

/// One bucket observation: model and market probability plus the outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Observation {
    pub event_id: String,
    pub kind: Kind,
    pub station: String,
    pub date: NaiveDate,
    pub label: String,
    pub model: f64,
    pub market: f64,
    pub outcome: f64,
}

/// Evaluate all events: returns one observation per bucket that had a
/// market price at the decision time.
pub fn observations(
    events: &[WeatherEvent],
    trades: &HashMap<String, Vec<BucketTrade>>,
    forecasts: &HashMap<String, StationForecasts>,
    cfg: &ModelConfig,
) -> Vec<Observation> {
    // Forecast errors per (station, kind): date -> actual - forecast.
    let mut errors: HashMap<(&str, Kind), BTreeMap<NaiveDate, f64>> = HashMap::new();
    for e in events {
        let Some(fc) = forecasts.get(&e.station) else {
            continue;
        };
        let Some(f) = fc.value(e.date, cfg.lead, e.kind) else {
            continue;
        };
        let Some(winner) = e.buckets.iter().find(|b| b.payout_yes >= 0.999) else {
            continue;
        };
        if let Some(actual) = bucket_value(winner.range, e.unit) {
            errors
                .entry((e.station.as_str(), e.kind))
                .or_default()
                .insert(e.date, actual - f);
        }
    }

    let mut out = Vec::new();
    for e in events {
        let Some(fc) = forecasts.get(&e.station) else {
            continue;
        };
        let Some(f) = fc.value(e.date, cfg.lead, e.kind) else {
            continue;
        };
        // Days resolved before the decision time: at least two days before
        // the event date (resolution happens the morning after a day).
        let Some(hist) = errors.get(&(e.station.as_str(), e.kind)) else {
            continue;
        };
        let from = e.date - chrono::Duration::days(cfg.history_days);
        let to = e.date - chrono::Duration::days(2);
        let past: Vec<f64> = hist.range(from..=to).map(|(_, v)| *v).collect();
        if past.len() < cfg.min_history {
            continue;
        }
        let n = past.len() as f64;
        let bias = past.iter().sum::<f64>() / n;
        let var = past.iter().map(|v| (v - bias).powi(2)).sum::<f64>() / (n - 1.0);
        let floor = match e.unit {
            Unit::F => cfg.sd_floor_f,
            Unit::C => cfg.sd_floor_c,
        };
        let sd = var.sqrt().max(floor);
        let mu = f + bias;

        let probs: Vec<f64> = e
            .buckets
            .iter()
            .map(|b| bucket_prob(b.range, mu, sd))
            .collect();
        let total: f64 = probs.iter().sum();
        let decision = decision_ts(e.date, cfg.lead, fc.utc_offset);
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScoreComparison {
    pub observations: usize,
    pub events: usize,
    pub brier_model: f64,
    pub brier_market: f64,
    /// brier_market - brier_model; > 0 means the model is more accurate.
    pub advantage: f64,
    pub advantage_lo: f64,
    pub advantage_hi: f64,
}

/// Brier scores of model and market, difference clustered by event.
pub fn compare(obs: &[Observation]) -> Option<ScoreComparison> {
    if obs.is_empty() {
        return None;
    }
    let n = obs.len() as f64;
    let d: Vec<f64> = obs
        .iter()
        .map(|o| (o.market - o.outcome).powi(2) - (o.model - o.outcome).powi(2))
        .collect();
    let mean = d.iter().sum::<f64>() / n;
    let mut clusters: HashMap<&str, f64> = HashMap::new();
    for (o, v) in obs.iter().zip(&d) {
        *clusters.entry(o.event_id.as_str()).or_default() += v - mean;
    }
    let se = clusters.values().map(|s| s * s).sum::<f64>().sqrt() / n;
    Some(ScoreComparison {
        observations: obs.len(),
        events: clusters.len(),
        brier_model: obs
            .iter()
            .map(|o| (o.model - o.outcome).powi(2))
            .sum::<f64>()
            / n,
        brier_market: obs
            .iter()
            .map(|o| (o.market - o.outcome).powi(2))
            .sum::<f64>()
            / n,
        advantage: mean,
        advantage_lo: mean - 1.96 * se,
        advantage_hi: mean + 1.96 * se,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StrategyResult {
    pub threshold: f64,
    pub bets: usize,
    pub events: usize,
    pub win_rate: f64,
    pub avg_price: f64,
    /// Taker: price + slippage + fee (`rate * p * (1 - p)` per share).
    pub roi_taker: f64,
    pub roi_taker_lo: f64,
    pub roi_taker_hi: f64,
    /// Maker (optimistic): filled at the observed price, no fee.
    pub roi_maker: f64,
    pub pnl_taker: f64,
}

/// Bet on every bucket where model and market differ by at least
/// `threshold`: YES when the model is higher, NO when it is lower.
pub fn strategy(
    obs: &[Observation],
    threshold: f64,
    fee_rate: f64,
    slippage_bps: f64,
    stake: f64,
) -> Option<StrategyResult> {
    // (event, price paid for the side, payout of the side)
    let bets: Vec<(&str, f64, f64)> = obs
        .iter()
        .filter_map(|o| {
            let (price, payout) = if o.model - o.market >= threshold {
                (o.market, o.outcome)
            } else if o.market - o.model >= threshold {
                (1.0 - o.market, 1.0 - o.outcome)
            } else {
                return None;
            };
            (0.02..=0.98)
                .contains(&price)
                .then_some((o.event_id.as_str(), price, payout))
        })
        .collect();
    if bets.is_empty() {
        return None;
    }
    let n = bets.len() as f64;
    let taker = |p: f64, y: f64| {
        let q = (p * (1.0 + slippage_bps / 10_000.0)).min(0.999);
        y / (q + fee_rate * q * (1.0 - q)) - 1.0
    };
    let rois: Vec<f64> = bets.iter().map(|(_, p, y)| taker(*p, *y)).collect();
    let roi = rois.iter().sum::<f64>() / n;
    let mut clusters: HashMap<&str, f64> = HashMap::new();
    for ((e, _, _), r) in bets.iter().zip(&rois) {
        *clusters.entry(e).or_default() += r - roi;
    }
    let se = clusters.values().map(|s| s * s).sum::<f64>().sqrt() / n;
    Some(StrategyResult {
        threshold,
        bets: bets.len(),
        events: clusters.len(),
        win_rate: bets.iter().filter(|(_, _, y)| *y >= 0.999).count() as f64 / n,
        avg_price: bets.iter().map(|(_, p, _)| p).sum::<f64>() / n,
        roi_taker: roi,
        roi_taker_lo: roi - 1.96 * se,
        roi_taker_hi: roi + 1.96 * se,
        roi_maker: bets.iter().map(|(_, p, y)| y / p - 1.0).sum::<f64>() / n,
        pnl_taker: stake * rois.iter().sum::<f64>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_titles_stations_dates() {
        assert_eq!(
            parse_kind("Highest temperature in Chicago on November 9?"),
            Some(Kind::High)
        );
        assert_eq!(
            parse_kind("Lowest temperature in Seattle on November 9?"),
            Some(Kind::Low)
        );
        assert_eq!(parse_kind("Where will it rain on November 8?"), None);
        assert_eq!(
            parse_station(
                "",
                "https://aviationweather.gov/api/data/metar?ids=KORD&format=raw"
            )
            .as_deref(),
            Some("KORD")
        );
        assert_eq!(
            parse_station(
                "reported at Chicago O'Hare Intl Airport (KORD) on 9 Nov",
                ""
            )
            .as_deref(),
            Some("KORD")
        );
        assert_eq!(
            parse_date("highest-temperature-in-chicago-on-november-9-2026"),
            NaiveDate::from_ymd_opt(2026, 11, 9)
        );
        assert_eq!(parse_date("nonsense"), None);
        assert_eq!(
            parse_station("", "https://www.weather.gov/wrh/timeseries?site=zhcc").as_deref(),
            Some("ZHCC")
        );
        assert_eq!(
            parse_station(
                "",
                "https://www.wunderground.com/history/daily/cn/jinan/ZSJN"
            )
            .as_deref(),
            Some("ZSJN")
        );
        assert_eq!(
            parse_station(
                "",
                "https://www.wunderground.com/history/daily/cn/jinan/date"
            ),
            None
        );
        assert_eq!(
            parse_station("recorded by NOAA at the Istanbul Airport", ""),
            None
        );
    }

    #[test]
    fn parses_bucket_labels() {
        let r = |lo, hi| Range { lo, hi };
        assert_eq!(
            parse_bucket("49°F or below"),
            Some((r(None, Some(49)), Unit::F))
        );
        assert_eq!(
            parse_bucket("50-51°F"),
            Some((r(Some(50), Some(51)), Unit::F))
        );
        assert_eq!(
            parse_bucket("68°F or higher"),
            Some((r(Some(68), None), Unit::F))
        );
        assert_eq!(parse_bucket("17°C"), Some((r(Some(17), Some(17)), Unit::C)));
        assert_eq!(
            parse_bucket("-2°C or below"),
            Some((r(None, Some(-2)), Unit::C))
        );
        assert_eq!(parse_bucket("-3°C"), Some((r(Some(-3), Some(-3)), Unit::C)));
        assert_eq!(
            parse_bucket("-1-0°F"),
            Some((r(Some(-1), Some(0)), Unit::F))
        );
        assert_eq!(parse_bucket("Yes"), None);
    }

    #[test]
    fn probabilities_sum_to_one_and_peak_at_forecast() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((norm_cdf(1.96) - 0.975).abs() < 1e-3);
        let buckets = [
            Range {
                lo: None,
                hi: Some(49),
            },
            Range {
                lo: Some(50),
                hi: Some(51),
            },
            Range {
                lo: Some(52),
                hi: Some(53),
            },
            Range {
                lo: Some(54),
                hi: None,
            },
        ];
        let p: Vec<f64> = buckets.iter().map(|r| bucket_prob(*r, 50.5, 1.5)).collect();
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        assert!(p[1] > p[0] && p[1] > p[2]);
    }

    #[test]
    fn bucket_values() {
        assert_eq!(
            bucket_value(
                Range {
                    lo: Some(50),
                    hi: Some(51)
                },
                Unit::F
            ),
            Some(50.5)
        );
        assert_eq!(
            bucket_value(
                Range {
                    lo: None,
                    hi: Some(49)
                },
                Unit::F
            ),
            Some(48.0)
        );
        assert_eq!(
            bucket_value(
                Range {
                    lo: Some(26),
                    hi: None
                },
                Unit::C
            ),
            Some(26.5)
        );
    }

    #[test]
    fn decision_times_are_local() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let utc_midnight = d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        // Chicago UTC-6: local midnight is 06:00 UTC.
        assert_eq!(decision_ts(d, 1, -6 * 3600), utc_midnight + 6 * 3600);
        // Lead 2: 06:00 local the day before.
        assert_eq!(
            decision_ts(d, 2, -6 * 3600),
            utc_midnight + 6 * 3600 - DAY + 6 * 3600
        );
    }

    fn event(id: &str, date: NaiveDate, winner: usize) -> WeatherEvent {
        let labels = ["49°F or below", "50-51°F", "52-53°F", "54°F or higher"];
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
    fn learns_only_from_resolved_days_and_uses_prices_before_decision() {
        let base = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let mut events = Vec::new();
        let mut fc = StationForecasts {
            utc_offset: 0,
            ..Default::default()
        };
        // 20 history days: forecast 49, actual bucket 50-51 (bias +1.5).
        for i in 0..20 {
            let d = base + chrono::Duration::days(i);
            events.push(event(&format!("h{i}"), d, 1));
            fc.daily.insert((d, 1), (49.0, 40.0));
        }
        // Target day: forecast 49 again, outcome 50-51.
        let target = base + chrono::Duration::days(20);
        events.push(event("t", target, 1));
        fc.daily.insert((target, 1), (49.0, 40.0));
        let forecasts = HashMap::from([("KORD".to_string(), fc)]);

        let decision = decision_ts(target, 1, 0);
        let trades = HashMap::from([(
            "t-1".to_string(),
            vec![
                BucketTrade {
                    condition_id: "t-1".into(),
                    timestamp: decision - 60,
                    yes_price: 0.30,
                    usd: 10.0,
                },
                // After the decision time: must be ignored.
                BucketTrade {
                    condition_id: "t-1".into(),
                    timestamp: decision + 60,
                    yes_price: 0.90,
                    usd: 10.0,
                },
                // Dust before the decision: ignored by the size filter.
                BucketTrade {
                    condition_id: "t-1".into(),
                    timestamp: decision - 30,
                    yes_price: 0.01,
                    usd: 0.5,
                },
            ],
        )]);
        let cfg = ModelConfig {
            lead: 1,
            history_days: 45,
            min_history: 10,
            sd_floor_f: 1.0,
            sd_floor_c: 0.6,
            min_price_usd: 2.0,
            max_price_age: None,
        };
        let obs = observations(&events, &trades, &forecasts, &cfg);
        let t: Vec<&Observation> = obs.iter().filter(|o| o.event_id == "t").collect();
        assert_eq!(t.len(), 1, "only the bucket with a price");
        assert_eq!(t[0].market, 0.30);
        assert!(
            t[0].model > 0.6,
            "bias learned from history: {}",
            t[0].model
        );

        let cmp = compare(&t.iter().map(|o| (*o).clone()).collect::<Vec<_>>()).unwrap();
        assert!(cmp.advantage > 0.0, "model beats market here");
        let s = strategy(
            &obs.iter()
                .filter(|o| o.event_id == "t")
                .cloned()
                .collect::<Vec<_>>(),
            0.1,
            0.05,
            0.0,
            10.0,
        )
        .unwrap();
        assert_eq!(s.bets, 1);
        let q = 0.30;
        assert!((s.roi_taker - (1.0 / (q + 0.05 * q * (1.0 - q)) - 1.0)).abs() < 1e-9);
        assert!((s.roi_maker - (1.0 / 0.30 - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn strategy_bets_no_when_market_is_too_high() {
        let o = Observation {
            event_id: "e".into(),
            kind: Kind::Low,
            station: "X".into(),
            date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            label: "l".into(),
            model: 0.10,
            market: 0.40,
            outcome: 0.0,
        };
        let s = strategy(&[o], 0.1, 0.0, 0.0, 10.0).unwrap();
        assert!((s.avg_price - 0.60).abs() < 1e-9, "NO bought at 1 - 0.40");
        assert_eq!(s.win_rate, 1.0);
    }
}
