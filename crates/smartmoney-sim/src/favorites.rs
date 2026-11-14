//! Strategy "favorites98" (ROADMAP Phase 5, pre-registered 2026-11-14):
//! follow large favourite buys at 95-98¢ in non-deadline markets with at
//! least a week to run, hold to resolution.

use smartmoney_core::config::FavoritesConfig;
use smartmoney_core::fees::is_deadline_question;
use smartmoney_store::records::{MarketRow, TradeRow};

/// Whether a stored trade triggers a paper follow; `Err` names the first
/// rule that rejected it (for logs and tests).
pub fn should_follow(
    trade: &TradeRow,
    market: &MarketRow,
    cfg: &FavoritesConfig,
    now: i64,
) -> Result<(), &'static str> {
    if !trade.side.eq_ignore_ascii_case("buy") {
        return Err("not a buy");
    }
    if trade.usdc_value < cfg.min_trigger_usd {
        return Err("below trigger size");
    }
    if trade.price < cfg.min_price || trade.price > cfg.max_price {
        return Err("outside price band");
    }
    let m = &market.market;
    if market.payouts.is_some() || m.closed {
        return Err("market already closed");
    }
    if is_deadline_question(m.question.as_deref().unwrap_or_default()) {
        return Err("deadline market");
    }
    let Some(end) = m.end_date else {
        return Err("no end date");
    };
    if end - trade.timestamp < cfg.min_days_to_end * 86_400 {
        return Err("ends too soon");
    }
    if now - trade.timestamp > 3600 {
        return Err("trigger too old");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::types::MarketCategory;
    use smartmoney_store::records::MarketRecord;

    fn trade() -> TradeRow {
        TradeRow {
            id: 1,
            transaction_hash: "0xh".into(),
            proxy_wallet: "0xw".into(),
            condition_id: "0xc".into(),
            token_id: "t".into(),
            side: "BUY".into(),
            outcome: Some("Yes".into()),
            outcome_index: Some(0),
            size: 2000.0,
            price: 0.96,
            usdc_value: 1920.0,
            timestamp: 1_000_000,
            title: None,
            slug: None,
            event_slug: None,
            ingested_at: 1_000_010,
        }
    }

    fn market(question: &str, end: i64) -> MarketRow {
        MarketRow {
            market: MarketRecord {
                condition_id: "0xc".into(),
                gamma_id: None,
                question: Some(question.into()),
                slug: None,
                event_slug: None,
                category: MarketCategory::Sports,
                gamma_category: None,
                tags: Vec::new(),
                volume_usd: None,
                volume_24h_usd: None,
                end_date: Some(end),
                closed: false,
                outcomes: vec!["Yes".into(), "No".into()],
                outcome_prices: Vec::new(),
                clob_token_ids: vec!["t".into(), "u".into()],
                neg_risk: None,
                resolution_status: None,
                raw_json: None,
                fetched_at: 0,
            },
            payouts: None,
            resolved_outcome_index: None,
            resolved_at: None,
        }
    }

    #[test]
    fn follows_the_registered_rule() {
        let cfg = FavoritesConfig::default();
        let t = trade();
        let far = 1_000_000 + 8 * 86_400;
        assert_eq!(
            should_follow(&t, &market("Will the Lakers win the title?", far), &cfg, t.timestamp + 60),
            Ok(())
        );
        // Each rule rejects.
        let mut sell = t.clone();
        sell.side = "SELL".into();
        assert!(should_follow(&sell, &market("x", far), &cfg, t.timestamp).is_err());
        let mut small = t.clone();
        small.usdc_value = 500.0;
        assert_eq!(
            should_follow(&small, &market("x", far), &cfg, t.timestamp),
            Err("below trigger size")
        );
        let mut cheap = t.clone();
        cheap.price = 0.90;
        assert_eq!(
            should_follow(&cheap, &market("x", far), &cfg, t.timestamp),
            Err("outside price band")
        );
        assert_eq!(
            should_follow(&t, &market("Will X resign by March 31?", far), &cfg, t.timestamp),
            Err("deadline market")
        );
        assert_eq!(
            should_follow(&t, &market("x", 1_000_000 + 3 * 86_400), &cfg, t.timestamp),
            Err("ends too soon")
        );
        let mut resolved = market("x", far);
        resolved.payouts = Some(vec![1.0, 0.0]);
        assert_eq!(
            should_follow(&t, &resolved, &cfg, t.timestamp),
            Err("market already closed")
        );
        assert_eq!(
            should_follow(&t, &market("x", far), &cfg, t.timestamp + 7200),
            Err("trigger too old")
        );
    }
}
