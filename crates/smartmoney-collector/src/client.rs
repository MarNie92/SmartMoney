//! HTTP client for the Polymarket Gamma, Data (v2) and CLOB APIs.
//! Read-only: this client never places orders.

use crate::api::{
    Activity, DataTrade, GammaEventWithMarkets, GammaMarket, Midpoint, OrderBook, Page, Position,
    PricesHistory, ResolutionRow, UserStats, UserStatsResponse,
};
use crate::ratelimit::{Backoff, RateLimiter};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use smartmoney_core::config::CollectorConfig;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 6;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("request to {url} failed: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("{url} returned HTTP {status}: {body}")]
    Status {
        url: String,
        status: StatusCode,
        body: String,
    },
    #[error("could not parse response from {url}: {source}")]
    Parse {
        url: String,
        source: serde_json::Error,
    },
}

/// A parsed value plus the raw JSON it came from.
#[derive(Debug, Clone)]
pub struct WithRaw<T> {
    pub value: T,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct PolymarketClient {
    http: reqwest::Client,
    limiter: Arc<RateLimiter>,
    backoff: Backoff,
    gamma_url: String,
    data_url: String,
    clob_url: String,
    /// When set, every response is cached on disk by URL (for immutable
    /// historical data in the backtest).
    cache_dir: Option<PathBuf>,
}

/// FNV-1a 64-bit hash, stable across runs (used for cache file names).
fn fnv1a(data: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn cache_key(url: &str, query: &[(&str, String)]) -> String {
    let mut parts: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
    parts.sort();
    format!("{:016x}", fnv1a(&format!("{url}?{}", parts.join("&"))))
}

impl PolymarketClient {
    pub fn new(cfg: &CollectorConfig) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("smartmoney/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(cfg.request_timeout_seconds))
            .gzip(true)
            .build()?;
        Ok(Self {
            http,
            limiter: Arc::new(RateLimiter::new(
                cfg.max_requests_per_10s,
                Duration::from_secs(10),
            )),
            backoff: Backoff {
                initial: Duration::from_millis(cfg.backoff_initial_ms),
                max: Duration::from_millis(cfg.backoff_max_ms),
            },
            gamma_url: cfg.gamma_api_url.trim_end_matches('/').to_owned(),
            data_url: cfg.data_api_url.trim_end_matches('/').to_owned(),
            clob_url: cfg.clob_api_url.trim_end_matches('/').to_owned(),
            cache_dir: None,
        })
    }

    /// Cache all responses in `dir` (created if missing). Only use this for
    /// data that does not change, such as closed markets.
    pub fn with_cache(mut self, dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        self.cache_dir = Some(dir);
        Ok(self)
    }

    /// Override the request budget (requests per 10 seconds).
    pub fn with_rate(mut self, max_per_10s: u32) -> Self {
        self.limiter = Arc::new(RateLimiter::new(max_per_10s, Duration::from_secs(10)));
        self
    }

    async fn get_value(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, ApiError> {
        let Some(dir) = &self.cache_dir else {
            return self.fetch_value(url, query).await;
        };
        let path = dir.join(format!("{}.json", cache_key(url, query)));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return Ok(value);
            }
        }
        let value = self.fetch_value(url, query).await?;
        if let Err(e) = tokio::fs::write(&path, value.to_string()).await {
            tracing::warn!(error = %e, path = %path.display(), "could not write cache file");
        }
        Ok(value)
    }

    /// GET with pacing, retries on 429 / 5xx / network errors.
    async fn fetch_value(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, ApiError> {
        let bytes = self.fetch_bytes(url, query).await?;
        serde_json::from_slice(&bytes).map_err(|source| ApiError::Parse {
            url: url.to_owned(),
            source,
        })
    }

    /// GET raw bytes with pacing and retries.
    async fn fetch_bytes(&self, url: &str, query: &[(&str, String)]) -> Result<Vec<u8>, ApiError> {
        let mut attempt = 0;
        loop {
            self.limiter.acquire().await;
            let result = self.http.get(url).query(query).send().await;
            let retry_delay = match result {
                Ok(resp) if resp.status().is_success() => {
                    let bytes = resp.bytes().await.map_err(|source| ApiError::Http {
                        url: url.to_owned(),
                        source,
                    })?;
                    return Ok(bytes.to_vec());
                }
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let retryable =
                        status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                    if !retryable || attempt + 1 >= MAX_ATTEMPTS {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ApiError::Status {
                            url: url.to_owned(),
                            status,
                            body: body.chars().take(300).collect(),
                        });
                    }
                    let delay = retry_after.unwrap_or_else(|| self.backoff.delay(attempt));
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        self.limiter.pause_for(delay);
                    }
                    tracing::warn!(%url, %status, ?delay, attempt, "retrying request");
                    delay
                }
                Err(source) => {
                    let retryable =
                        source.is_timeout() || source.is_connect() || source.is_request();
                    if !retryable || attempt + 1 >= MAX_ATTEMPTS {
                        return Err(ApiError::Http {
                            url: url.to_owned(),
                            source,
                        });
                    }
                    let delay = self.backoff.delay(attempt);
                    tracing::warn!(%url, error = %source, ?delay, attempt, "retrying request");
                    delay
                }
            };
            tokio::time::sleep(retry_delay).await;
            attempt += 1;
        }
    }

    async fn get<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<WithRaw<T>, ApiError> {
        let raw = self.get_value(url, query).await?;
        let value = T::deserialize(&raw).map_err(|source| ApiError::Parse {
            url: url.to_owned(),
            source,
        })?;
        Ok(WithRaw { value, raw })
    }

    /// Global trade feed, newest first, filtered by minimum USDC value.
    /// Returns the parsed page and the raw JSON of each row.
    pub async fn trades_page(
        &self,
        min_usd: f64,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<(Page<DataTrade>, Vec<serde_json::Value>), ApiError> {
        let mut query = vec![
            ("filter_type", "CASH".to_owned()),
            ("filter_amount", format!("{min_usd}")),
            ("limit", limit.to_string()),
        ];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_owned()));
        }
        let res: WithRaw<Page<DataTrade>> = self
            .get(&format!("{}/v2/trades", self.data_url), &query)
            .await?;
        let raws = res
            .raw
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        Ok((res.value, raws))
    }

    pub async fn user_stats(&self, user: &str) -> Result<WithRaw<Option<UserStats>>, ApiError> {
        let res: WithRaw<UserStatsResponse> = self
            .get(
                &format!("{}/v2/user-stats", self.data_url),
                &[("user", user.to_owned())],
            )
            .await?;
        Ok(WithRaw {
            value: res.value.data,
            raw: res.raw,
        })
    }

    /// Earliest activity row of a wallet (full history).
    pub async fn first_activity(&self, user: &str) -> Result<Option<Activity>, ApiError> {
        let res: WithRaw<Page<Activity>> = self
            .get(
                &format!("{}/v2/activity", self.data_url),
                &[
                    ("user", user.to_owned()),
                    ("sort_direction", "ASC".to_owned()),
                    ("start", "1".to_owned()),
                    ("limit", "1".to_owned()),
                ],
            )
            .await?;
        Ok(res.value.data.into_iter().next())
    }

    /// Open positions of a wallet, largest current value first.
    pub async fn positions(&self, user: &str, limit: u32) -> Result<Vec<Position>, ApiError> {
        let res: WithRaw<Page<Position>> = self
            .get(
                &format!("{}/v2/positions", self.data_url),
                &[
                    ("user", user.to_owned()),
                    ("sort_by", "CURRENT_VALUE".to_owned()),
                    ("limit", limit.to_string()),
                ],
            )
            .await?;
        Ok(res.value.data)
    }

    pub async fn resolution(&self, condition_id: &str) -> Result<Option<ResolutionRow>, ApiError> {
        let res: WithRaw<Page<ResolutionRow>> = self
            .get(
                &format!("{}/v2/resolutions", self.data_url),
                &[("condition", condition_id.to_owned())],
            )
            .await?;
        Ok(res.value.data.into_iter().next())
    }

    /// Gamma market by condition id, including tags. Returns the market and
    /// its raw JSON object.
    pub async fn gamma_market(
        &self,
        condition_id: &str,
    ) -> Result<Option<WithRaw<GammaMarket>>, ApiError> {
        let raw = self
            .get_value(
                &format!("{}/markets", self.gamma_url),
                &[
                    ("condition_ids", condition_id.to_owned()),
                    ("include_tag", "true".to_owned()),
                    // Without this Gamma only returns open markets.
                    ("closed", "any".to_owned()),
                ],
            )
            .await?;
        let first = raw.as_array().and_then(|a| a.first()).cloned();
        let Some(obj) = first else { return Ok(None) };
        let value = GammaMarket::deserialize(&obj).map_err(|source| ApiError::Parse {
            url: format!("{}/markets", self.gamma_url),
            source,
        })?;
        Ok(Some(WithRaw { value, raw: obj }))
    }

    pub async fn book(&self, token_id: &str) -> Result<OrderBook, ApiError> {
        Ok(self
            .get(
                &format!("{}/book", self.clob_url),
                &[("token_id", token_id.to_owned())],
            )
            .await?
            .value)
    }

    pub async fn midpoint(&self, token_id: &str) -> Result<Option<f64>, ApiError> {
        let res: WithRaw<Midpoint> = self
            .get(
                &format!("{}/midpoint", self.clob_url),
                &[("token_id", token_id.to_owned())],
            )
            .await?;
        Ok(res.value.mid.parse().ok())
    }

    /// Price history (`t` epoch seconds) between two timestamps.
    pub async fn prices_history(
        &self,
        token_id: &str,
        start_ts: i64,
        end_ts: i64,
        fidelity_minutes: u32,
    ) -> Result<PricesHistory, ApiError> {
        Ok(self
            .get(
                &format!("{}/prices-history", self.clob_url),
                &[
                    ("market", token_id.to_owned()),
                    ("startTs", start_ts.to_string()),
                    ("endTs", end_ts.to_string()),
                    ("fidelity", fidelity_minutes.to_string()),
                ],
            )
            .await?
            .value)
    }
    /// Closed events with a tag (Gamma `/events`), most recently ended first.
    /// `end_date_max` is an RFC 3339 timestamp.
    pub async fn closed_events(
        &self,
        tag_slug: &str,
        end_date_max: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<GammaEventWithMarkets>, ApiError> {
        let url = format!("{}/events", self.gamma_url);
        let raw = self
            .get_value(
                &url,
                &[
                    ("tag_slug", tag_slug.to_owned()),
                    ("closed", "true".to_owned()),
                    ("order", "endDate".to_owned()),
                    ("ascending", "false".to_owned()),
                    ("end_date_max", end_date_max.to_owned()),
                    ("offset", offset.to_string()),
                    ("limit", limit.to_string()),
                ],
            )
            .await?;
        let events = raw.as_array().cloned().unwrap_or_default();
        Ok(events
            .iter()
            .filter_map(|e| GammaEventWithMarkets::from_value(e).ok())
            .collect())
    }

    /// One page (newest first, up to 1000) of all taker trades in a market.
    pub async fn market_trades_page(
        &self,
        condition_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<DataTrade>, ApiError> {
        let mut query = vec![
            ("condition", condition_id.to_owned()),
            ("limit", "1000".to_owned()),
        ];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_owned()));
        }
        Ok(self
            .get(&format!("{}/v2/trades", self.data_url), &query)
            .await?
            .value)
    }

    /// One page of a market's trades with USDC value >= ,
    /// newest first (cursor pagination).
    pub async fn market_trades_min_cash_page(
        &self,
        condition_id: &str,
        min_cash: f64,
        cursor: Option<&str>,
    ) -> Result<Page<DataTrade>, ApiError> {
        let mut query = vec![
            ("condition", condition_id.to_owned()),
            ("filter_type", "CASH".to_owned()),
            ("filter_amount", format!("{min_cash}")),
            ("limit", "1000".to_owned()),
        ];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_owned()));
        }
        Ok(self
            .get(&format!("{}/v2/trades", self.data_url), &query)
            .await?
            .value)
    }

    /// Up to 1000 most recent BUY trades of a wallet within `[start, end]`.
    pub async fn user_buys(
        &self,
        user: &str,
        start: i64,
        end: i64,
    ) -> Result<Vec<DataTrade>, ApiError> {
        let res: WithRaw<Page<DataTrade>> = self
            .get(
                &format!("{}/v2/trades", self.data_url),
                &[
                    ("user", user.to_owned()),
                    ("side", "BUY".to_owned()),
                    ("start", start.max(1).to_string()),
                    ("end", end.to_string()),
                    ("limit", "1000".to_owned()),
                ],
            )
            .await?;
        Ok(res.value.data)
    }
    /// One page (newest first, up to 1000) of a wallet's taker trades
    /// between `start` and `end` (epoch seconds, inclusive).
    pub async fn user_trades_page(
        &self,
        user: &str,
        start: i64,
        end: i64,
        cursor: Option<&str>,
    ) -> Result<Page<DataTrade>, ApiError> {
        let mut query = vec![
            ("user", user.to_owned()),
            ("start", start.max(1).to_string()),
            ("end", end.to_string()),
            ("limit", "1000".to_owned()),
        ];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_owned()));
        }
        Ok(self
            .get(&format!("{}/v2/trades", self.data_url), &query)
            .await?
            .value)
    }

    /// Gamma markets for several condition ids at once (open or closed).
    /// Malformed entries are skipped.
    pub async fn gamma_markets_batch(
        &self,
        condition_ids: &[String],
    ) -> Result<Vec<GammaMarket>, ApiError> {
        if condition_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut query: Vec<(&str, String)> = condition_ids
            .iter()
            .map(|c| ("condition_ids", c.clone()))
            .collect();
        query.push(("closed", "any".to_owned()));
        query.push(("limit", condition_ids.len().max(1).to_string()));
        let raw = self
            .get_value(&format!("{}/markets", self.gamma_url), &query)
            .await?;
        Ok(raw
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| GammaMarket::deserialize(m).ok())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Wallet addresses from the leaderboard (time period: day, week, month,
    /// all; sort: PNL or VOLUME), best first.
    pub async fn leaderboard(
        &self,
        time_period: &str,
        sort_by: &str,
        limit: u32,
    ) -> Result<Vec<String>, ApiError> {
        let raw = self
            .get_value(
                &format!("{}/v2/leaderboard", self.data_url),
                &[
                    ("time_period", time_period.to_owned()),
                    ("sort_by", sort_by.to_owned()),
                    ("category", "overall".to_owned()),
                    ("limit", limit.to_string()),
                ],
            )
            .await?;
        Ok(raw
            .get("data")
            .and_then(|d| d.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.get("user_id")?.as_str())
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default())
    }
    /// One page (newest first, up to 1000) of all taker trades of an event.
    pub async fn event_trades_page(
        &self,
        event_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<DataTrade>, ApiError> {
        let mut query = vec![
            ("event_id", event_id.to_owned()),
            ("limit", "1000".to_owned()),
        ];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_owned()));
        }
        Ok(self
            .get(&format!("{}/v2/trades", self.data_url), &query)
            .await?
            .value)
    }

    /// GET any JSON URL through the same pacing, retries and (optional)
    /// cache, e.g. weather data for the backtest.
    pub async fn get_json(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, ApiError> {
        self.get_value(url, query).await
    }
    /// Closed events with a tag via cursor pagination (Gamma
    /// events/keyset), most recently ended first. Returns the events and
    /// the cursor of the next page.
    pub async fn closed_events_keyset(
        &self,
        tag_slug: &str,
        end_date_max: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<GammaEventWithMarkets>, Option<String>), ApiError> {
        let mut query = vec![
            ("tag_slug", tag_slug.to_owned()),
            ("closed", "true".to_owned()),
            ("order", "endDate".to_owned()),
            ("ascending", "false".to_owned()),
            ("end_date_max", end_date_max.to_owned()),
            ("limit", limit.to_string()),
        ];
        if let Some(c) = cursor {
            query.push(("after_cursor", c.to_owned()));
        }
        let raw = self
            .get_value(&format!("{}/events/keyset", self.gamma_url), &query)
            .await?;
        let events = raw
            .get("events")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| GammaEventWithMarkets::from_value(e).ok())
                    .collect()
            })
            .unwrap_or_default();
        let next = raw
            .get("next_cursor")
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        Ok((events, next))
    }
    /// GET a text response (e.g. CSV) through pacing, retries and the
    /// optional cache. Returns the text and whether it came from the cache,
    /// so callers can add extra politeness delays only for real requests.
    pub async fn get_text(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<(String, bool), ApiError> {
        let path = self
            .cache_dir
            .as_ref()
            .map(|dir| dir.join(format!("{}.txt", cache_key(url, query))));
        if let Some(path) = &path {
            if let Ok(text) = tokio::fs::read_to_string(path).await {
                return Ok((text, true));
            }
        }
        let text = String::from_utf8_lossy(&self.fetch_bytes(url, query).await?).into_owned();
        if let Some(path) = &path {
            if let Err(e) = tokio::fs::write(path, &text).await {
                tracing::warn!(error = %e, path = %path.display(), "could not write cache file");
            }
        }
        Ok((text, false))
    }
}
