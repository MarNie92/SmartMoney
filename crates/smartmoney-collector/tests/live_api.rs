//! Tests against the live Polymarket APIs. Ignored by default (network).
//! Run with: `cargo test -p smartmoney-collector --test live_api -- --ignored`

use smartmoney_collector::{Collector, PolymarketClient};
use smartmoney_core::config::CollectorConfig;
use smartmoney_store::Store;

fn client() -> PolymarketClient {
    PolymarketClient::new(&CollectorConfig::default()).unwrap()
}

#[tokio::test]
#[ignore = "hits the live Polymarket API"]
async fn live_endpoints_parse() {
    let c = client();
    let (page, raws) = c.trades_page(2000.0, 5, None).await.unwrap();
    assert!(!page.data.is_empty(), "no large trades in the feed");
    assert_eq!(page.data.len(), raws.len());
    let t = &page.data[0];
    assert!(
        t.usdc_value() >= 2000.0,
        "CASH filter not applied: {}",
        t.usdc_value()
    );
    assert!(t.timestamp > 1_700_000_000, "timestamp not in seconds");

    let market = c
        .gamma_market(&t.condition_id)
        .await
        .unwrap()
        .expect("market");
    assert_eq!(market.value.condition_id, t.condition_id);
    assert!(!market.value.clob_token_ids.is_empty());

    let stats = c.user_stats(&t.proxy_wallet).await.unwrap();
    assert!(stats.raw.get("data").is_some());
    c.positions(&t.proxy_wallet, 5).await.unwrap();
    c.first_activity(&t.proxy_wallet).await.unwrap();

    let book = c.book(&t.token_id).await.unwrap().to_snapshot(0);
    if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
        assert!(bid <= ask, "book not normalized: bid {bid} > ask {ask}");
    }
    let now = chrono::Utc::now().timestamp();
    let history = c
        .prices_history(&t.token_id, now - 86_400, now, 60)
        .await
        .unwrap();
    if let Some(p) = history.history.first() {
        assert!(
            p.t > 1_700_000_000 && p.t < now + 3600,
            "history t not seconds"
        );
    }
}

#[tokio::test]
#[ignore = "hits the live Polymarket API"]
async fn live_gamma_returns_closed_markets_and_resolutions() {
    let c = client();
    // Known resolved market (2026-11-08): payouts [0, 1].
    let cid = "0x0ac4e680df3ead2ef35cc91db624696c1b7021123deb24b3eea8219589e87533";
    let m = c
        .gamma_market(cid)
        .await
        .unwrap()
        .expect("closed market via closed=any");
    assert_eq!(m.value.closed, Some(true));
    let r = c
        .resolution(cid)
        .await
        .unwrap()
        .unwrap()
        .to_resolution()
        .unwrap();
    assert_eq!(r.winning_outcome_index(), Some(1));
}

#[tokio::test]
#[ignore = "hits the live Polymarket API"]
async fn live_poll_into_temp_db() {
    let store = Store::open_in_memory().await.unwrap();
    let collector = Collector::new(client(), store.clone(), CollectorConfig::default(), None);
    let stats = collector.poll_once().await.unwrap();
    println!("live poll: {stats:?}");
    assert!(stats.seen > 0);
    let trades = store.recent_trades(3).await.unwrap();
    for t in &trades {
        println!(
            "{} {} {:.0}$ @ {} {:?}",
            t.proxy_wallet, t.side, t.usdc_value, t.price, t.title
        );
        assert!(store.market(&t.condition_id).await.unwrap().is_some());
        assert!(store.wallet(&t.proxy_wallet).await.unwrap().is_some());
    }
}
