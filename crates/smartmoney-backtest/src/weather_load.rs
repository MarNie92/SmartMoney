//! Data loading for the weather backtest (Gamma events, station
//! coordinates, Open-Meteo previous-run forecasts, event trades).

use crate::dataset::gamma_payouts;
use crate::weather::{
    parse_bucket, parse_date, parse_kind, parse_station, Bucket, BucketTrade, StationForecasts,
    Unit, WeatherEvent,
};
use chrono::NaiveDate;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use smartmoney_collector::{ApiError, PolymarketClient};
use std::collections::{HashMap, HashSet};

const DAY: i64 = 86_400;
const EVENTS_PAGE: usize = 100;
const MAX_EVENT_PAGES: usize = 200;
const CONCURRENCY: usize = 8;
const STATION_URL: &str = "https://aviationweather.gov/api/data/stationinfo";
const FORECAST_URL: &str = "https://previous-runs-api.open-meteo.com/v1/forecast";

#[derive(Debug, Default, Clone, Serialize)]
pub struct WeatherLoadStats {
    pub events_seen: usize,
    pub events_used: usize,
    pub events_skipped: usize,
    pub stations: usize,
    pub stations_without_forecast: usize,
    pub trades: usize,
}

pub struct WeatherData {
    pub events: Vec<WeatherEvent>,
    pub trades: HashMap<String, Vec<BucketTrade>>,
    pub forecasts: HashMap<String, StationForecasts>,
    pub stats: WeatherLoadStats,
}

/// Convert a Gamma event into a weather event (None if anything is off).
fn to_event(e: smartmoney_collector::api::GammaEventWithMarkets, now: i64) -> Option<WeatherEvent> {
    let kind = parse_kind(e.title.as_deref()?)?;
    let station = parse_station(
        e.description.as_deref().unwrap_or_default(),
        e.resolution_source.as_deref().unwrap_or_default(),
    )?;
    let date = parse_date(e.slug.as_deref()?)?;
    let mut unit = None;
    let mut buckets = Vec::new();
    for m in e.markets {
        let label = m.group_item_title.clone()?;
        let (range, u) = parse_bucket(&label)?;
        if unit.is_some_and(|x| x != u) {
            return None;
        }
        unit = Some(u);
        let yes_index = m
            .outcomes
            .iter()
            .position(|o| o.eq_ignore_ascii_case("yes"))?;
        let record = m.into_record(None, now);
        let payouts = gamma_payouts(&record)?;
        buckets.push(Bucket {
            condition_id: record.condition_id,
            label,
            range,
            yes_index,
            payout_yes: *payouts.get(yes_index)?,
        });
    }
    let winners = buckets.iter().filter(|b| b.payout_yes >= 0.999).count();
    (winners == 1 && buckets.len() >= 3).then_some(WeatherEvent {
        event_id: e.id?,
        kind,
        station,
        date,
        unit: unit?,
        buckets,
    })
}

async fn load_events(
    client: &PolymarketClient,
    days: i64,
    now: i64,
    stats: &mut WeatherLoadStats,
) -> Result<Vec<WeatherEvent>, ApiError> {
    let end_max = chrono::DateTime::from_timestamp(now - now.rem_euclid(DAY), 0)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let oldest = now - days * DAY;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    // Cursor pagination: offset paging on /events stops at ~2000 events.
    let mut cursor: Option<String> = None;
    'pages: for page in 0..MAX_EVENT_PAGES {
        let (events, next) = client
            .closed_events_keyset("weather", &end_max, cursor.as_deref(), EVENTS_PAGE)
            .await?;
        if events.is_empty() {
            break;
        }
        for e in events {
            let end = e
                .end_date
                .as_deref()
                .and_then(smartmoney_collector::api::parse_datetime);
            if end.is_some_and(|t| t < oldest) {
                break 'pages;
            }
            if !e.title.as_deref().is_some_and(|t| parse_kind(t).is_some()) {
                continue;
            }
            stats.events_seen += 1;
            if !e.id.as_ref().is_some_and(|id| seen.insert(id.clone())) {
                continue;
            }
            match to_event(e, now) {
                Some(w) => out.push(w),
                None => stats.events_skipped += 1,
            }
        }
        if page % 10 == 9 {
            tracing::info!(page, events = out.len(), "weather events loaded");
        }
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    Ok(out)
}

async fn station_coords(
    client: &PolymarketClient,
    stations: &[String],
) -> Result<HashMap<String, (f64, f64)>, ApiError> {
    let mut out = HashMap::new();
    for chunk in stations.chunks(40) {
        let raw = client
            .get_json(
                STATION_URL,
                &[("ids", chunk.join(",")), ("format", "json".to_owned())],
            )
            .await?;
        for s in raw.as_array().into_iter().flatten() {
            let (Some(id), Some(lat), Some(lon)) = (
                s.get("icaoId").and_then(|v| v.as_str()),
                s.get("lat").and_then(|v| v.as_f64()),
                s.get("lon").and_then(|v| v.as_f64()),
            ) else {
                continue;
            };
            out.insert(id.to_owned(), (lat, lon));
        }
    }
    Ok(out)
}

/// Daily max/min per local date for leads 1 and 2 from hourly values.
pub fn daily_extremes(raw: &serde_json::Value) -> StationForecasts {
    let mut fc = StationForecasts {
        utc_offset: raw
            .get("utc_offset_seconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        ..Default::default()
    };
    let Some(hourly) = raw.get("hourly") else {
        return fc;
    };
    let times: Vec<&str> = hourly
        .get("time")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str()).collect())
        .unwrap_or_default();
    for (lead, key) in [
        (1u8, "temperature_2m_previous_day1"),
        (2u8, "temperature_2m_previous_day2"),
    ] {
        let Some(values) = hourly.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        let mut per_day: HashMap<NaiveDate, Vec<f64>> = HashMap::new();
        for (t, v) in times.iter().zip(values) {
            let (Some(date), Some(v)) = (
                t.get(..10)
                    .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
                v.as_f64(),
            ) else {
                continue;
            };
            per_day.entry(date).or_default().push(v);
        }
        for (date, vals) in per_day {
            // Require nearly complete days.
            if vals.len() >= 20 {
                let max = vals.iter().copied().fold(f64::MIN, f64::max);
                let min = vals.iter().copied().fold(f64::MAX, f64::min);
                fc.daily.insert((date, lead), (max, min));
            }
        }
    }
    fc
}

pub async fn load(
    client: &PolymarketClient,
    days: i64,
    history_days: i64,
    now: i64,
) -> Result<WeatherData, ApiError> {
    let mut stats = WeatherLoadStats::default();
    let events = load_events(client, days, now, &mut stats).await?;
    stats.events_used = events.len();
    tracing::info!(events = events.len(), "weather events parsed");

    // Stations and their unit (one unit per station in practice).
    let mut units: HashMap<String, Unit> = HashMap::new();
    for e in &events {
        units.entry(e.station.clone()).or_insert(e.unit);
    }
    let mut stations: Vec<String> = units.keys().cloned().collect();
    stations.sort();
    stats.stations = stations.len();
    let coords = station_coords(client, &stations).await?;

    let (Some(first), Some(last)) = (
        events.iter().map(|e| e.date).min(),
        events.iter().map(|e| e.date).max(),
    ) else {
        return Ok(WeatherData {
            events,
            trades: HashMap::new(),
            forecasts: HashMap::new(),
            stats,
        });
    };
    let start = (first - chrono::Duration::days(history_days + 5))
        .format("%Y-%m-%d")
        .to_string();
    let end = last.format("%Y-%m-%d").to_string();

    let mut forecasts = HashMap::new();
    for station in &stations {
        let Some((lat, lon)) = coords.get(station) else {
            stats.stations_without_forecast += 1;
            continue;
        };
        let unit = match units[station] {
            Unit::F => "fahrenheit",
            Unit::C => "celsius",
        };
        let raw = client
            .get_json(
                FORECAST_URL,
                &[
                    ("latitude", format!("{lat:.4}")),
                    ("longitude", format!("{lon:.4}")),
                    (
                        "hourly",
                        "temperature_2m_previous_day1,temperature_2m_previous_day2".to_owned(),
                    ),
                    ("temperature_unit", unit.to_owned()),
                    ("timezone", "auto".to_owned()),
                    ("start_date", start.clone()),
                    ("end_date", end.clone()),
                ],
            )
            .await;
        match raw {
            Ok(raw) => {
                forecasts.insert(station.clone(), daily_extremes(&raw));
            }
            Err(e) => {
                tracing::warn!(%station, error = %e, "forecast skipped");
                stats.stations_without_forecast += 1;
            }
        }
    }

    // Trades per event, mapped to YES prices of each bucket.
    let yes_index: HashMap<String, usize> = events
        .iter()
        .flat_map(|e| {
            e.buckets
                .iter()
                .map(|b| (b.condition_id.clone(), b.yes_index))
        })
        .collect();
    let ids: Vec<String> = events.iter().map(|e| e.event_id.clone()).collect();
    let mut results = stream::iter(ids)
        .map(|id| async move {
            let mut all = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..20 {
                let page = client.event_trades_page(&id, cursor.as_deref()).await?;
                all.extend(page.data.clone());
                match page.next_cursor() {
                    Some(c) => cursor = Some(c.to_owned()),
                    None => break,
                }
            }
            Ok::<_, ApiError>(all)
        })
        .buffer_unordered(CONCURRENCY);
    let mut trades: HashMap<String, Vec<BucketTrade>> = HashMap::new();
    let mut done = 0;
    while let Some(r) = results.next().await {
        done += 1;
        match r {
            Ok(rows) => {
                for t in rows {
                    let (Some(yes), Some(idx)) = (yes_index.get(&t.condition_id), t.outcome_index)
                    else {
                        continue;
                    };
                    let yes_price = if idx as usize == *yes {
                        t.price
                    } else {
                        1.0 - t.price
                    };
                    stats.trades += 1;
                    trades
                        .entry(t.condition_id.clone())
                        .or_default()
                        .push(BucketTrade {
                            condition_id: t.condition_id,
                            timestamp: t.timestamp,
                            yes_price,
                            usd: t.size * t.price,
                        });
                }
            }
            Err(e) => tracing::warn!(error = %e, "event trades skipped"),
        }
        if done % 500 == 0 {
            tracing::info!(done, "event trades loaded");
        }
    }

    Ok(WeatherData {
        events,
        trades,
        forecasts,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn daily_extremes_per_local_date_and_lead() {
        let mut times = Vec::new();
        let mut d1 = Vec::new();
        for h in 0..24 {
            times.push(format!("2026-11-09T{h:02}:00"));
            d1.push(json!(50.0 + h as f64 / 2.0));
        }
        // Incomplete next day: skipped.
        times.push("2026-11-10T00:00".into());
        d1.push(json!(99.0));
        let raw = json!({
            "utc_offset_seconds": -21600,
            "hourly": {"time": times, "temperature_2m_previous_day1": d1},
        });
        let fc = daily_extremes(&raw);
        let d = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        assert_eq!(fc.utc_offset, -21600);
        assert_eq!(fc.daily.get(&(d, 1)), Some(&(61.5, 50.0)));
        assert!(!fc.daily.contains_key(&(d, 2)));
        assert!(!fc.daily.contains_key(&(d.succ_opt().unwrap(), 1)));
    }
}
