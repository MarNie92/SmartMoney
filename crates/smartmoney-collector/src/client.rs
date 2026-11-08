//! HTTP client for the Polymarket Gamma, Data (v2) and CLOB APIs.
//! Read-only: this client never places orders.

use crate::api::{
    Activity, DataTrade, GammaMarket, Midpoint, OrderBook, Page, Position, PricesHistory,
    ResolutionRow, UserStats, UserStatsResponse,
};
use crate::ratelimit::{Backoff, RateLimiter};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use smartmoney_core::config::CollectorConfig;
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
        })
    }

    /// GET with pacing, retries on 429 / 5xx / network errors.
    async fn get_value(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, ApiError> {
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
                    return serde_json::from_slice(&bytes).map_err(|source| ApiError::Parse {
                        url: url.to_owned(),
                        source,
                    });
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
}
