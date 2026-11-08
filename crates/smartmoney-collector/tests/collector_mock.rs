//! End-to-end collector test against a mock HTTP server serving recorded
//! live responses.

use smartmoney_collector::service::STATE_LAST_TRADE_TS;
use smartmoney_collector::{Collector, CollectorEvent, PolymarketClient};
use smartmoney_core::config::CollectorConfig;
use smartmoney_core::MarketCategory;
use smartmoney_store::Store;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn json(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "application/json")
}

const WALLET: &str = "0x3e3d362468e8676196bc374203ec242f40f5d674";
const CONDITION: &str = "0x131d29203f63524c6ee473e43036ba6c1c808b24687d5031b6ddab29f822fe66";

async fn setup() -> (MockServer, Collector, Store, mpsc::Receiver<CollectorEvent>) {
    let server = MockServer::start().await;
    // A single page: drop the cursor so pagination stops.
    let trades = fixture("trades.json").replace("\"has_more\":true", "\"has_more\":false");
    Mock::given(method("GET"))
        .and(path("/v2/trades"))
        .and(query_param("filter_type", "CASH"))
        .and(query_param("filter_amount", "2000"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(trades, "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/markets"))
        .and(query_param("include_tag", "true"))
        .respond_with(json("gamma_markets.json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/user-stats"))
        .respond_with(json("user_stats.json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/positions"))
        .respond_with(json("positions.json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/book"))
        .respond_with(json("book.json"))
        .mount(&server)
        .await;

    let cfg = CollectorConfig {
        gamma_api_url: server.uri(),
        data_api_url: server.uri(),
        clob_api_url: server.uri(),
        max_requests_per_10s: 10_000,
        backoff_initial_ms: 1,
        backoff_max_ms: 5,
        ..Default::default()
    };
    let store = Store::open_in_memory().await.unwrap();
    let (tx, rx) = mpsc::channel(16);
    let collector = Collector::new(
        PolymarketClient::new(&cfg).unwrap(),
        store.clone(),
        cfg,
        Some(tx),
    );
    (server, collector, store, rx)
}

#[tokio::test]
async fn poll_stores_and_enriches_trades() {
    let (_server, collector, store, mut rx) = setup().await;

    let stats = collector.poll_once().await.unwrap();
    // Fixture trades fill the default `trades.json` sample: both above $2,000.
    assert_eq!(stats.pages, 1);
    assert_eq!(stats.new_trades.len(), 2);
    assert_eq!(store.count_trades().await.unwrap(), 2);
    assert_eq!(
        store
            .get_state(STATE_LAST_TRADE_TS)
            .await
            .unwrap()
            .as_deref(),
        Some("1794155770")
    );

    let market = store.market(CONDITION).await.unwrap().unwrap();
    assert_eq!(market.market.category, MarketCategory::CryptoPrice);
    assert_eq!(market.market.volume_24h_usd, Some(7326.400767));

    let wallet = store.wallet(WALLET).await.unwrap().unwrap();
    assert_eq!(wallet.join_date, Some(1772823560));
    assert_eq!(wallet.markets_traded, Some(777));
    assert_eq!(wallet.trade_count, Some(1137));
    assert!(wallet.total_positions_usd.unwrap() > 0.0);
    assert_eq!(store.wallet_positions(WALLET).await.unwrap().len(), 2);

    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| matches!(
        e,
        CollectorEvent::TradeStored {
            snapshot_id: Some(_),
            ..
        }
    )));

    // Second poll: same trades again -> nothing new, no duplicate events.
    let stats = collector.poll_once().await.unwrap();
    assert!(stats.new_trades.is_empty());
    assert_eq!(store.count_trades().await.unwrap(), 2);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn retries_after_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/book"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/book"))
        .respond_with(json("book.json"))
        .mount(&server)
        .await;
    let cfg = CollectorConfig {
        clob_api_url: server.uri(),
        max_requests_per_10s: 10_000,
        backoff_initial_ms: 1,
        backoff_max_ms: 5,
        ..Default::default()
    };
    let client = PolymarketClient::new(&cfg).unwrap();
    let book = client.book("123").await.unwrap();
    assert_eq!(book.to_snapshot(0).best_bid(), Some(0.94));
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/book"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = CollectorConfig {
        clob_api_url: server.uri(),
        max_requests_per_10s: 10_000,
        ..Default::default()
    };
    let client = PolymarketClient::new(&cfg).unwrap();
    assert!(client.book("123").await.is_err());
}
