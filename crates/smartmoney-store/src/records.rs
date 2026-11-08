//! Plain data records exchanged with the store.

use smartmoney_core::{MarketCategory, Side};

/// A trade to insert (mapped from Data API `/v2/trades`).
#[derive(Debug, Clone, PartialEq)]
pub struct NewTrade {
    pub transaction_hash: String,
    pub proxy_wallet: String,
    pub condition_id: String,
    pub token_id: String,
    pub side: Side,
    pub outcome: Option<String>,
    pub outcome_index: Option<i64>,
    /// Shares.
    pub size: f64,
    /// USDC per share.
    pub price: f64,
    /// Epoch seconds.
    pub timestamp: i64,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub event_slug: Option<String>,
    pub raw_json: Option<String>,
}

impl NewTrade {
    pub fn usdc_value(&self) -> f64 {
        self.size * self.price
    }
}

/// A stored trade.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TradeRow {
    pub id: i64,
    pub transaction_hash: String,
    pub proxy_wallet: String,
    pub condition_id: String,
    pub token_id: String,
    pub side: String,
    pub outcome: Option<String>,
    pub outcome_index: Option<i64>,
    pub size: f64,
    pub price: f64,
    pub usdc_value: f64,
    pub timestamp: i64,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub event_slug: Option<String>,
    pub ingested_at: i64,
}

/// Market data from Gamma. Resolution fields are written separately.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketRecord {
    pub condition_id: String,
    pub gamma_id: Option<String>,
    pub question: Option<String>,
    pub slug: Option<String>,
    pub event_slug: Option<String>,
    pub category: MarketCategory,
    pub gamma_category: Option<String>,
    pub tags: Vec<String>,
    pub volume_usd: Option<f64>,
    pub volume_24h_usd: Option<f64>,
    pub end_date: Option<i64>,
    pub closed: bool,
    pub outcomes: Vec<String>,
    pub outcome_prices: Vec<f64>,
    pub clob_token_ids: Vec<String>,
    pub neg_risk: Option<bool>,
    pub resolution_status: Option<String>,
    pub raw_json: Option<String>,
    pub fetched_at: i64,
}

/// A stored market including resolution state.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketRow {
    pub market: MarketRecord,
    /// USDC per share for each outcome once resolved.
    pub payouts: Option<Vec<f64>>,
    pub resolved_outcome_index: Option<i64>,
    pub resolved_at: Option<i64>,
}

/// Market resolution (Data API `/v2/resolutions`).
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub status: String,
    /// USDC per share for each outcome.
    pub payouts: Vec<f64>,
    pub resolved_at: Option<i64>,
}

impl Resolution {
    /// Index of the outcome that pays the full dollar, if any (no split).
    pub fn winning_outcome_index(&self) -> Option<i64> {
        let winners: Vec<usize> = self
            .payouts
            .iter()
            .enumerate()
            .filter(|(_, p)| (**p - 1.0).abs() < 1e-9)
            .map(|(i, _)| i)
            .collect();
        match winners.as_slice() {
            [single] => Some(*single as i64),
            _ => None,
        }
    }
}

/// Enriched wallet information.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WalletRecord {
    pub address: String,
    pub name: Option<String>,
    pub pseudonym: Option<String>,
    /// `/v2/user-stats` join_date (epoch seconds).
    pub join_date: Option<i64>,
    /// Earliest activity seen (epoch seconds).
    pub first_activity_at: Option<i64>,
    /// Distinct markets traded.
    pub markets_traded: Option<i64>,
    /// Number of fills across all markets.
    pub trade_count: Option<i64>,
    pub total_positions_usd: Option<f64>,
    pub largest_position_usd: Option<f64>,
    pub raw_stats_json: Option<String>,
    pub enriched_at: i64,
}

impl WalletRecord {
    /// Best known wallet creation time.
    pub fn created_at(&self) -> Option<i64> {
        match (self.join_date, self.first_activity_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// One open position of a wallet.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionRecord {
    pub token_id: String,
    pub condition_id: String,
    pub current_size: Option<f64>,
    pub current_value: Option<f64>,
    pub total_cost: Option<f64>,
}

/// Normalized order book: bids best (highest) first, asks best (lowest) first.
#[derive(Debug, Clone, PartialEq)]
pub struct BookSnapshot {
    pub token_id: String,
    pub taken_at: i64,
    /// (price, size in shares)
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

impl BookSnapshot {
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.0)
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winning_outcome() {
        let r = |p: Vec<f64>| Resolution {
            status: "resolved".into(),
            payouts: p,
            resolved_at: None,
        };
        assert_eq!(r(vec![0.0, 1.0]).winning_outcome_index(), Some(1));
        assert_eq!(r(vec![1.0, 0.0]).winning_outcome_index(), Some(0));
        assert_eq!(r(vec![0.5, 0.5]).winning_outcome_index(), None);
        assert_eq!(r(vec![]).winning_outcome_index(), None);
    }

    #[test]
    fn wallet_created_at_prefers_earliest() {
        let mut w = WalletRecord {
            join_date: Some(200),
            first_activity_at: Some(100),
            ..Default::default()
        };
        assert_eq!(w.created_at(), Some(100));
        w.first_activity_at = None;
        assert_eq!(w.created_at(), Some(200));
        w.join_date = None;
        assert_eq!(w.created_at(), None);
    }
}
