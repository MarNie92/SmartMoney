//! Polymarket API response models and parsers.
//!
//! Verified against live responses on 2026-11-08. Deviations from the docs:
//! - CLOB `/book` levels are sorted with the best price *last* (bids
//!   ascending, asks descending) and `timestamp` is in milliseconds; we
//!   normalize to best-first ourselves.
//! - CLOB `/midpoint` returns `{"mid": "..."}` (docs: `mid_price`).
//! - Gamma `outcomes`, `outcomePrices`, `clobTokenIds` are JSON arrays
//!   encoded as strings; `category` is empty, tags need `include_tag=true`.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::de::{self, Deserializer};
use serde::Deserialize;
use smartmoney_core::{classify_market, Side};
use smartmoney_store::{BookSnapshot, MarketRecord, NewTrade, PositionRecord, Resolution};

/// Cursor-paginated Data API v2 response.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pagination {
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Cursor for the next page, if there is one.
    pub fn next_cursor(&self) -> Option<&str> {
        self.pagination
            .as_ref()
            .filter(|p| p.has_more)
            .and_then(|p| p.next_cursor.as_deref())
            .filter(|c| !c.is_empty())
    }
}

/// `/v2/trades` row.
#[derive(Debug, Clone, Deserialize)]
pub struct DataTrade {
    pub proxy_wallet: String,
    pub side: String,
    pub token_id: String,
    pub condition_id: String,
    pub size: f64,
    pub price: f64,
    pub timestamp: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub event_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub outcome_index: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pseudonym: Option<String>,
    pub transaction_hash: String,
}

/// `outcome_index` value the API uses for "unlabeled".
const UNLABELED_OUTCOME: i64 = 999;

impl DataTrade {
    pub fn usdc_value(&self) -> f64 {
        self.size * self.price
    }

    pub fn into_new_trade(self, raw_json: Option<String>) -> Result<NewTrade, String> {
        let side: Side = self.side.parse()?;
        Ok(NewTrade {
            transaction_hash: self.transaction_hash,
            proxy_wallet: self.proxy_wallet.to_lowercase(),
            condition_id: self.condition_id,
            token_id: self.token_id,
            side,
            outcome: non_empty(self.outcome),
            outcome_index: self.outcome_index.filter(|i| *i != UNLABELED_OUTCOME),
            size: self.size,
            price: self.price,
            timestamp: self.timestamp,
            title: non_empty(self.title),
            slug: non_empty(self.slug),
            event_slug: non_empty(self.event_slug),
            raw_json,
        })
    }
}

/// `/v2/user-stats` response.
#[derive(Debug, Clone, Deserialize)]
pub struct UserStatsResponse {
    pub data: Option<UserStats>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserStats {
    pub proxy_wallet: String,
    /// Distinct markets traded.
    #[serde(default)]
    pub trades: Option<i64>,
    #[serde(default)]
    pub join_date: Option<i64>,
    #[serde(default)]
    pub all_time_pnl: Option<serde_json::Value>,
}

impl UserStats {
    /// Number of fills across all markets (`all_time_pnl.trade_count`).
    pub fn trade_count(&self) -> Option<i64> {
        self.all_time_pnl.as_ref()?.get("trade_count")?.as_i64()
    }
}

/// `/v2/activity` row (only the fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    pub timestamp: i64,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pseudonym: Option<String>,
}

/// `/v2/positions` row (only the fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub condition_id: String,
    #[serde(default)]
    pub current_size: Option<f64>,
    #[serde(default)]
    pub current_value: Option<f64>,
    #[serde(default)]
    pub total_cost_usdc: Option<f64>,
}

impl From<Position> for PositionRecord {
    fn from(p: Position) -> Self {
        PositionRecord {
            token_id: p.token_id,
            condition_id: p.condition_id,
            current_size: p.current_size,
            current_value: p.current_value,
            total_cost: p.total_cost_usdc,
        }
    }
}

/// `/v2/resolutions` row.
#[derive(Debug, Clone, Deserialize)]
pub struct ResolutionRow {
    #[serde(default)]
    pub condition_id: Option<String>,
    pub status: String,
    /// Micro-USDC per share for each outcome.
    #[serde(default)]
    pub payouts: Option<Vec<i64>>,
    #[serde(default)]
    pub resolved_at: Option<String>,
}

const MICRO_USDC: f64 = 1_000_000.0;

impl ResolutionRow {
    /// Converted resolution, only if the market is fully resolved with payouts.
    pub fn to_resolution(&self) -> Option<Resolution> {
        if self.status != "resolved" {
            return None;
        }
        let payouts = self.payouts.as_ref().filter(|p| !p.is_empty())?;
        Some(Resolution {
            status: self.status.clone(),
            payouts: payouts.iter().map(|p| *p as f64 / MICRO_USDC).collect(),
            resolved_at: self.resolved_at.as_deref().and_then(parse_datetime),
        })
    }
}

/// Gamma market.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaMarket {
    pub id: String,
    pub condition_id: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tags: Vec<GammaTag>,
    #[serde(default)]
    pub volume_num: Option<f64>,
    #[serde(default)]
    pub volume24hr: Option<f64>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub closed: Option<bool>,
    #[serde(default, deserialize_with = "json_string_list")]
    pub outcomes: Vec<String>,
    #[serde(default, deserialize_with = "json_string_list")]
    pub outcome_prices: Vec<String>,
    #[serde(default, deserialize_with = "json_string_list")]
    pub clob_token_ids: Vec<String>,
    #[serde(default)]
    pub uma_resolution_status: Option<String>,
    #[serde(default)]
    pub neg_risk: Option<bool>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub events: Vec<GammaEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GammaTag {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GammaEvent {
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tags: Vec<GammaTag>,
}

impl GammaMarket {
    pub fn tag_slugs(&self) -> Vec<String> {
        let mut slugs: Vec<String> = self
            .tags
            .iter()
            .chain(self.events.iter().flat_map(|e| e.tags.iter()))
            .filter_map(|t| t.slug.clone())
            .collect();
        slugs.sort();
        slugs.dedup();
        slugs
    }

    pub fn into_record(self, raw_json: Option<String>, fetched_at: i64) -> MarketRecord {
        let tags = self.tag_slugs();
        let question = non_empty(self.question);
        let category = classify_market(&tags, question.as_deref().unwrap_or_default());
        MarketRecord {
            condition_id: self.condition_id,
            gamma_id: Some(self.id),
            question,
            slug: non_empty(self.slug),
            event_slug: self.events.iter().find_map(|e| e.slug.clone()),
            category,
            gamma_category: non_empty(self.category),
            tags,
            volume_usd: self.volume_num,
            volume_24h_usd: self.volume24hr,
            end_date: self.end_date.as_deref().and_then(parse_datetime),
            closed: self.closed.unwrap_or(false),
            outcomes: self.outcomes,
            outcome_prices: self
                .outcome_prices
                .iter()
                .filter_map(|p| p.trim().parse().ok())
                .collect(),
            clob_token_ids: self.clob_token_ids,
            neg_risk: self.neg_risk,
            resolution_status: non_empty(self.uma_resolution_status),
            raw_json,
            fetched_at,
        }
    }
}

/// CLOB `/book` response.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderBook {
    pub asset_id: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub bids: Vec<BookLevel>,
    #[serde(default)]
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookLevel {
    pub price: String,
    pub size: String,
}

impl OrderBook {
    /// Normalize to best-first levels; drops unparsable or empty levels.
    pub fn to_snapshot(&self, taken_at: i64) -> BookSnapshot {
        let parse = |levels: &[BookLevel]| -> Vec<(f64, f64)> {
            levels
                .iter()
                .filter_map(|l| Some((l.price.parse().ok()?, l.size.parse().ok()?)))
                .filter(|(_, s): &(f64, f64)| *s > 0.0)
                .collect()
        };
        let mut bids = parse(&self.bids);
        let mut asks = parse(&self.asks);
        bids.sort_by(|a, b| b.0.total_cmp(&a.0));
        asks.sort_by(|a, b| a.0.total_cmp(&b.0));
        BookSnapshot {
            token_id: self.asset_id.clone(),
            taken_at,
            bids,
            asks,
        }
    }
}

/// CLOB `/midpoint` response.
#[derive(Debug, Clone, Deserialize)]
pub struct Midpoint {
    #[serde(alias = "mid_price")]
    pub mid: String,
}

/// CLOB `/prices-history` response; `t` is epoch seconds.
#[derive(Debug, Clone, Deserialize)]
pub struct PricesHistory {
    #[serde(default)]
    pub history: Vec<PricePoint>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PricePoint {
    pub t: i64,
    pub p: f64,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Accepts a JSON array, a string containing a JSON array, or null.
fn json_string_list<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(d)?;
    let array = match value {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => return Ok(Vec::new()),
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str::<Vec<serde_json::Value>>(&s).map_err(de::Error::custom)?
        }
        Some(serde_json::Value::Array(a)) => a,
        Some(other) => return Err(de::Error::custom(format!("expected list, got {other}"))),
    };
    Ok(array
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        })
        .collect())
}

/// Parse RFC 3339 and Gamma's "2026-11-08 16:36:53.76+00" style to epoch seconds.
pub fn parse_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    let normalized = {
        let t = s.replacen(' ', "T", 1);
        // "+00" -> "+00:00"
        match t.rfind(['+', '-']) {
            Some(i) if i > 10 && t.len() - i == 3 => format!("{t}:00"),
            _ => t,
        }
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(&normalized) {
        return Some(dt.timestamp());
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(dt.and_utc().timestamp());
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc).timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::MarketCategory;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn parses_trades_page() {
        let page: Page<DataTrade> = serde_json::from_str(&fixture("trades.json")).unwrap();
        assert_eq!(page.data.len(), 2);
        assert!(page.next_cursor().is_some());
        let t = page.data[0].clone();
        assert_eq!(t.side, "BUY");
        assert_eq!(t.timestamp, 1794155770);
        assert!((t.usdc_value() - 5034.4322 * 0.9633658302).abs() < 1e-6);
        let nt = t.into_new_trade(None).unwrap();
        assert_eq!(nt.side, Side::Buy);
        assert_eq!(nt.outcome.as_deref(), Some("No"));
        assert_eq!(nt.outcome_index, Some(1));
    }

    #[test]
    fn last_page_has_no_cursor() {
        let page: Page<DataTrade> = serde_json::from_str(
            r#"{"data":[],"pagination":{"limit":2,"offset":0,"has_more":false,"next_cursor":null}}"#,
        )
        .unwrap();
        assert!(page.next_cursor().is_none());
    }

    #[test]
    fn unlabeled_outcome_and_bad_side() {
        let mut t: DataTrade = serde_json::from_str(
            r#"{"proxy_wallet":"0xAB","side":"SELL","token_id":"1","condition_id":"0xc",
                "size":1,"price":0.5,"timestamp":1,"outcome":"","outcome_index":999,
                "transaction_hash":"0xt"}"#,
        )
        .unwrap();
        let nt = t.clone().into_new_trade(None).unwrap();
        assert_eq!(nt.outcome_index, None);
        assert_eq!(nt.outcome, None);
        assert_eq!(nt.proxy_wallet, "0xab");
        t.side = "HOLD".into();
        assert!(t.into_new_trade(None).is_err());
    }

    #[test]
    fn parses_gamma_market() {
        let markets: Vec<GammaMarket> =
            serde_json::from_str(&fixture("gamma_markets.json")).unwrap();
        let m = markets.into_iter().next().unwrap();
        assert_eq!(m.outcomes, vec!["Yes", "No"]);
        assert_eq!(m.clob_token_ids.len(), 2);
        assert!(m.tag_slugs().contains(&"crypto-prices".to_string()) || !m.tags.is_empty());
        let r = m.into_record(None, 42);
        assert_eq!(r.category, MarketCategory::CryptoPrice);
        assert_eq!(r.outcome_prices.len(), 2);
        assert_eq!(r.end_date, Some(1794200400)); // 2026-11-09T05:00:00Z
        assert_eq!(
            r.event_slug.as_deref(),
            Some("what-price-will-bitcoin-hit-november-2-8-2026")
        );
        assert!(!r.closed);
    }

    #[test]
    fn gamma_lists_accept_arrays_and_null() {
        let m: GammaMarket = serde_json::from_str(
            r#"{"id":"1","conditionId":"0xc","outcomes":["A","B"],"outcomePrices":null,
                "clobTokenIds":"","tags":null,"events":null}"#,
        )
        .unwrap();
        assert_eq!(m.outcomes, vec!["A", "B"]);
        assert!(m.outcome_prices.is_empty());
        assert!(m.clob_token_ids.is_empty());
        assert!(m.tags.is_empty());
    }

    #[test]
    fn parses_user_stats_and_activity() {
        let s: UserStatsResponse = serde_json::from_str(&fixture("user_stats.json")).unwrap();
        let s = s.data.unwrap();
        assert_eq!(s.trades, Some(777));
        assert_eq!(s.join_date, Some(1772823560));
        assert_eq!(s.trade_count(), Some(1137));

        let unknown: UserStatsResponse = serde_json::from_str(r#"{"data":null}"#).unwrap();
        assert!(unknown.data.is_none());

        let a: Page<Activity> = serde_json::from_str(&fixture("activity.json")).unwrap();
        assert_eq!(a.data[0].timestamp, 1772913587);
        assert_eq!(a.data[0].kind.as_deref(), Some("TRADE"));
    }

    #[test]
    fn parses_positions() {
        let p: Page<Position> = serde_json::from_str(&fixture("positions.json")).unwrap();
        let rec: PositionRecord = p.data[0].clone().into();
        assert_eq!(rec.current_value, Some(4978.0505));
        assert_eq!(rec.total_cost, Some(5016.38516));
    }

    #[test]
    fn parses_resolution() {
        let r: Page<ResolutionRow> = serde_json::from_str(&fixture("resolutions.json")).unwrap();
        let res = r.data[0].to_resolution().unwrap();
        assert_eq!(res.payouts, vec![0.0, 1.0]);
        assert_eq!(res.winning_outcome_index(), Some(1));
        assert_eq!(res.resolved_at, Some(1794155805)); // 2026-11-08T16:36:45Z

        let pending: ResolutionRow =
            serde_json::from_str(r#"{"status":"proposed","payouts":null}"#).unwrap();
        assert!(pending.to_resolution().is_none());
    }

    #[test]
    fn normalizes_order_book() {
        let b: OrderBook = serde_json::from_str(&fixture("book.json")).unwrap();
        let s = b.to_snapshot(7);
        assert_eq!(s.best_bid(), Some(0.94));
        assert_eq!(s.best_ask(), Some(0.96));
        assert_eq!(s.bids, vec![(0.94, 7513.0), (0.93, 1006.27), (0.92, 70.0)]);
        assert_eq!(s.asks.last(), Some(&(0.98, 12552.03)));
        assert_eq!(s.taken_at, 7);
    }

    #[test]
    fn parses_midpoint_and_history() {
        let m: Midpoint = serde_json::from_str(r#"{"mid":"0.95"}"#).unwrap();
        assert_eq!(m.mid, "0.95");
        let m: Midpoint = serde_json::from_str(r#"{"mid_price":"0.5"}"#).unwrap();
        assert_eq!(m.mid, "0.5");
        let h: PricesHistory = serde_json::from_str(&fixture("prices_history.json")).unwrap();
        assert_eq!(h.history.len(), 3);
        assert_eq!(h.history[0].t, 1794070847);
    }

    #[test]
    fn parses_datetimes() {
        assert_eq!(parse_datetime("2026-11-09T05:00:00Z"), Some(1794200400));
        assert_eq!(
            parse_datetime("2026-11-08 16:36:53.761924+00"),
            Some(1794155813)
        );
        assert_eq!(parse_datetime("2026-11-09"), Some(1794182400));
        assert_eq!(parse_datetime("garbage"), None);
    }
}
