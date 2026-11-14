#![forbid(unsafe_code)]

//! Private, mobile-first web dashboard (server-rendered, minimal JS).
//!
//! Intended for access over Tailscale. Without a configured token the server
//! refuses to bind to anything but a loopback address.

mod assets;
mod auth;
pub mod format;
mod views;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use smartmoney_collector::service::STATE_LAST_TRADE_TS;
use smartmoney_core::config::{DashboardConfig, ScoringConfig};
use smartmoney_scorer::effective_threshold;
use smartmoney_store::Store;
use std::net::SocketAddr;
use tokio::sync::watch;

const FEED_LIMIT: i64 = 100;
const DAY: i64 = 86_400;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub scoring: ScoringConfig,
    /// Dashboard token; `None` means no protection (loopback only).
    pub token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("invalid dashboard bind address {0:?}")]
    BadAddress(String),
    #[error(
        "refusing to bind the dashboard to {0} without SMARTMONEY_DASHBOARD_TOKEN; \
         set a token or bind to 127.0.0.1"
    )]
    TokenRequired(SocketAddr),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(feed))
        .route("/live", get(live))
        .route("/wallet/{address}", get(wallet))
        .route("/sim", get(sim))
        .route("/static/app.css", get(css))
        .route("/static/app.js", get(js))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Html(views::not_found().into_string()),
            )
        })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .with_state(state)
}

/// Check the bind address against the token policy.
pub fn resolve_bind(cfg: &DashboardConfig) -> Result<SocketAddr, WebError> {
    let addr: SocketAddr = cfg
        .bind_address
        .parse()
        .map_err(|_| WebError::BadAddress(cfg.bind_address.clone()))?;
    if cfg.token.is_none() && !addr.ip().is_loopback() {
        return Err(WebError::TokenRequired(addr));
    }
    Ok(addr)
}

/// Serve the dashboard until `shutdown` flips to true.
pub async fn serve(
    cfg: &DashboardConfig,
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), WebError> {
    let addr = resolve_bind(cfg)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, protected = state.token.is_some(), "dashboard listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            while !*shutdown.borrow() {
                if shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct FeedQuery {
    min: Option<f64>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "dashboard request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Datenbankfehler, siehe Daemon-Log.",
    )
        .into_response()
}

async fn load_feed(
    state: &AppState,
    min: f64,
) -> Result<(views::Status, Vec<smartmoney_store::ScoredTrade>), smartmoney_store::StoreError> {
    let now = now();
    let threshold = effective_threshold(&state.store, &state.scoring).await;
    let status = views::Status {
        now,
        trades_24h: state.store.count_trades_since(now - DAY).await?,
        alerts_24h: state.store.count_scores_since(now - DAY, threshold).await?,
        threshold,
        last_poll: state.store.state_updated_at(STATE_LAST_TRADE_TS).await?,
    };
    let rows = state.store.recent_scores(min, FEED_LIMIT).await?;
    Ok((status, rows))
}

async fn feed(State(state): State<AppState>, Query(q): Query<FeedQuery>) -> Response {
    let min = q.min.unwrap_or(0.0).clamp(0.0, 100.0);
    match load_feed(&state, min).await {
        Ok((status, rows)) => {
            Html(views::feed_page(&status, &rows, &state.scoring.weights, min).into_string())
                .into_response()
        }
        Err(e) => server_error(e),
    }
}

async fn sim(State(state): State<AppState>) -> Response {
    let now = chrono::Utc::now().timestamp();
    let accounts = match state.store.sim_accounts().await {
        Ok(a) => a,
        Err(e) => return server_error(e),
    };
    let mut data = Vec::new();
    for account in accounts {
        let positions = match state.store.sim_positions(account.id, 40).await {
            Ok(p) => p,
            Err(e) => return server_error(e),
        };
        let equity = match state.store.sim_equity(account.id, 1).await {
            Ok(series) => series.last().map(|(_, e)| *e),
            Err(e) => return server_error(e),
        };
        data.push(views::SimAccountView {
            account,
            positions,
            equity,
        });
    }
    Html(views::sim_page(&data, now).into_string()).into_response()
}

async fn live(State(state): State<AppState>, Query(q): Query<FeedQuery>) -> Response {
    let min = q.min.unwrap_or(0.0).clamp(0.0, 100.0);
    match load_feed(&state, min).await {
        Ok((status, rows)) => (
            [(header::CACHE_CONTROL, "no-store")],
            Html(views::live_region(&status, &rows, &state.scoring.weights, min).into_string()),
        )
            .into_response(),
        Err(e) => server_error(e),
    }
}

async fn wallet(State(state): State<AppState>, Path(address): Path<String>) -> Response {
    let address = address.to_lowercase();
    if !(address.starts_with("0x")
        && address.len() == 42
        && address[2..].chars().all(|c| c.is_ascii_hexdigit()))
    {
        return (
            StatusCode::NOT_FOUND,
            Html(views::not_found().into_string()),
        )
            .into_response();
    }
    let result = async {
        let wallet = state.store.wallet(&address).await?;
        let scores = state.store.wallet_scores(&address, 50).await?;
        let mut positions = Vec::new();
        for p in state.store.wallet_positions(&address).await? {
            let title = state
                .store
                .market(&p.condition_id)
                .await?
                .and_then(|m| m.market.question);
            positions.push(views::PositionView {
                title,
                value: p.current_value,
                cost: p.total_cost,
            });
        }
        let threshold = effective_threshold(&state.store, &state.scoring).await;
        Ok::<_, smartmoney_store::StoreError>((wallet, positions, scores, threshold))
    }
    .await;
    match result {
        Ok((w, positions, scores, threshold)) => Html(
            views::wallet_page(
                &address,
                w.as_ref(),
                &positions,
                &scores,
                &state.scoring.weights,
                threshold,
                now(),
            )
            .into_string(),
        )
        .into_response(),
        Err(e) => server_error(e),
    }
}

async fn css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        assets::CSS,
    )
}

async fn js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        assets::JS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use smartmoney_core::{MarketCategory, Secret, Side};
    use smartmoney_store::{MarketRecord, NewScore, NewTrade, WalletRecord};
    use tower::ServiceExt;

    const WALLET: &str = "0x544aa000db6bef61d6163759f8475860b940041b";

    async fn seeded_state(token: Option<&str>) -> AppState {
        let store = Store::open_in_memory().await.unwrap();
        let ts = now() - 120;
        let trade_id = store
            .insert_trade(
                &NewTrade {
                    transaction_hash: "0xt".into(),
                    proxy_wallet: WALLET.into(),
                    condition_id: "0xm".into(),
                    token_id: "tok".into(),
                    side: Side::Buy,
                    outcome: Some("Yes".into()),
                    outcome_index: Some(0),
                    size: 100_000.0,
                    price: 0.04,
                    timestamp: ts,
                    title: Some("Will the CEO <resign>?".into()),
                    slug: Some("ceo-resign".into()),
                    event_slug: Some("ceo".into()),
                    raw_json: None,
                },
                ts,
            )
            .await
            .unwrap()
            .unwrap();
        store
            .upsert_market(&MarketRecord {
                condition_id: "0xm".into(),
                gamma_id: None,
                question: Some("Will the CEO <resign>?".into()),
                slug: None,
                event_slug: Some("ceo".into()),
                category: MarketCategory::Personnel,
                gamma_category: None,
                tags: vec![],
                volume_usd: None,
                volume_24h_usd: Some(10_000.0),
                end_date: None,
                closed: false,
                outcomes: vec![],
                outcome_prices: vec![],
                clob_token_ids: vec![],
                neg_risk: None,
                resolution_status: None,
                raw_json: None,
                fetched_at: ts,
            })
            .await
            .unwrap();
        store
            .upsert_wallet(&WalletRecord {
                address: WALLET.into(),
                join_date: Some(ts - 86_400),
                markets_traded: Some(1),
                enriched_at: ts,
                ..Default::default()
            })
            .await
            .unwrap();
        store
            .upsert_score(&NewScore {
                trade_id,
                proxy_wallet: WALLET.into(),
                condition_id: "0xm".into(),
                total: 90.0,
                wallet_freshness: 25.0,
                price_asymmetry: 25.0,
                relative_size: 20.0,
                concentration: 15.0,
                market_type: 15.0,
                cluster_bonus: 0.0,
                details_json: r#"{"wallet_freshness":{"inputs":{"age_days":1.0,"markets_traded":1}},"relative_size":{"share":0.4,"volume_24h_usd":10000},"concentration":{"share":1.0},"cluster_bonus":{"fresh_wallets":1,"window_minutes":30}}"#.into(),
                scorer_version: 1,
                created_at: ts,
            })
            .await
            .unwrap();
        store
            .set_state(STATE_LAST_TRADE_TS, &ts.to_string(), now())
            .await
            .unwrap();
        AppState {
            store,
            scoring: ScoringConfig::default(),
            token: token.map(str::to_owned),
        }
    }

    async fn get_body(app: Router, uri: &str) -> (StatusCode, String, axum::http::HeaderMap) {
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            String::from_utf8_lossy(&bytes).into_owned(),
            headers,
        )
    }

    #[tokio::test]
    async fn feed_renders_scored_trades_escaped() {
        let app = router(seeded_state(None).await);
        let (status, body, _) = get_body(app, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Will the CEO &lt;resign&gt;?"),
            "title must be escaped"
        );
        assert!(body.contains("class=\"row alert\""));
        assert!(body.contains("$4,000"));
        assert!(body.contains("4¢"));
        assert!(body.contains("Personalie"));
        assert!(body.contains("40 % des 24h-Volumens"));
        assert!(
            body.contains("1 große Trades in 24 h") || body.contains(">1</strong> große Trades")
        );
    }

    #[tokio::test]
    async fn min_filter_and_live_fragment() {
        let state = seeded_state(None).await;
        let (_, body, _) = get_body(router(state.clone()), "/?min=95").await;
        assert!(body.contains("Noch keine Trades ab Score 95"));
        let (status, body, headers) = get_body(router(state), "/live?min=0").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("<html"), "fragment only");
        assert!(body.contains("data-id="));
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn wallet_page_and_validation() {
        let state = seeded_state(None).await;
        let (status, body, _) = get_body(router(state.clone()), &format!("/wallet/{WALLET}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Wallet 0x544a…041b"));
        assert!(body.contains("Bewertete Trades"));
        let (status, _, _) = get_body(router(state), "/wallet/not-an-address").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_protection() {
        let state = seeded_state(Some("s3cret")).await;
        let (status, body, _) = get_body(router(state.clone()), "/").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("Token eingeben"));
        assert!(!body.contains("CEO"), "no data before login");

        let (status, _, headers) = get_body(router(state.clone()), "/?token=wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(headers.get(header::SET_COOKIE).is_none());

        let (status, _, headers) = get_body(router(state.clone()), "/?token=s3cret&min=40").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "/?min=40");
        assert!(headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("HttpOnly"));

        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::COOKIE, "sm_token=s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (status, _, _) = get_body(router(state), "/healthz").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn bind_policy() {
        let mut cfg = DashboardConfig::default();
        assert!(resolve_bind(&cfg).is_ok());
        cfg.bind_address = "0.0.0.0:8080".into();
        assert!(matches!(
            resolve_bind(&cfg),
            Err(WebError::TokenRequired(_))
        ));
        cfg.token = Some(Secret::new("t"));
        assert!(resolve_bind(&cfg).is_ok());
        cfg.bind_address = "nonsense".into();
        assert!(matches!(resolve_bind(&cfg), Err(WebError::BadAddress(_))));
    }
}
