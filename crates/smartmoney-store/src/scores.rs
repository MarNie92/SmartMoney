//! Score persistence and queries.

use crate::{Result, Store};

/// A score to store (one per trade; re-scoring replaces it).
#[derive(Debug, Clone, PartialEq)]
pub struct NewScore {
    pub trade_id: i64,
    pub proxy_wallet: String,
    pub condition_id: String,
    pub total: f64,
    pub wallet_freshness: f64,
    pub price_asymmetry: f64,
    pub relative_size: f64,
    pub concentration: f64,
    pub market_type: f64,
    pub cluster_bonus: f64,
    pub details_json: String,
    pub scorer_version: i64,
    pub created_at: i64,
}

/// A stored score joined with its trade and market for display.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ScoredTrade {
    pub score_id: i64,
    pub trade_id: i64,
    pub total: f64,
    pub wallet_freshness: f64,
    pub price_asymmetry: f64,
    pub relative_size: f64,
    pub concentration: f64,
    pub market_type: f64,
    pub cluster_bonus: f64,
    pub details_json: String,
    pub scored_at: i64,
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
    pub category: Option<String>,
}

const SCORED_SELECT: &str = "SELECT s.id AS score_id, s.trade_id, s.total, s.wallet_freshness,
        s.price_asymmetry, s.relative_size, s.concentration, s.market_type, s.cluster_bonus,
        s.details_json, s.created_at AS scored_at, t.proxy_wallet, t.condition_id, t.token_id,
        t.side, t.outcome, t.outcome_index, t.size, t.price, t.usdc_value, t.timestamp,
        t.title, t.slug, COALESCE(t.event_slug, m.event_slug) AS event_slug, m.category
    FROM scores s
    JOIN trades t ON t.id = s.trade_id
    LEFT JOIN markets m ON m.condition_id = t.condition_id";

impl Store {
    /// Insert or replace the score of a trade; returns the score id.
    pub async fn upsert_score(&self, s: &NewScore) -> Result<i64> {
        let id = sqlx::query_scalar(
            "INSERT INTO scores (
                trade_id, proxy_wallet, condition_id, total, wallet_freshness, price_asymmetry,
                relative_size, concentration, market_type, cluster_bonus, details_json,
                scorer_version, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (trade_id) DO UPDATE SET
                total = excluded.total,
                wallet_freshness = excluded.wallet_freshness,
                price_asymmetry = excluded.price_asymmetry,
                relative_size = excluded.relative_size,
                concentration = excluded.concentration,
                market_type = excluded.market_type,
                cluster_bonus = excluded.cluster_bonus,
                details_json = excluded.details_json,
                scorer_version = excluded.scorer_version,
                created_at = excluded.created_at
            RETURNING id",
        )
        .bind(s.trade_id)
        .bind(&s.proxy_wallet)
        .bind(&s.condition_id)
        .bind(s.total)
        .bind(s.wallet_freshness)
        .bind(s.price_asymmetry)
        .bind(s.relative_size)
        .bind(s.concentration)
        .bind(s.market_type)
        .bind(s.cluster_bonus)
        .bind(&s.details_json)
        .bind(s.scorer_version)
        .bind(s.created_at)
        .fetch_one(self.pool())
        .await?;
        Ok(id)
    }

    pub async fn scored_trade(&self, score_id: i64) -> Result<Option<ScoredTrade>> {
        let row = sqlx::query_as::<_, ScoredTrade>(&format!("{SCORED_SELECT} WHERE s.id = ?"))
            .bind(score_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row)
    }

    pub async fn score_for_trade(&self, trade_id: i64) -> Result<Option<ScoredTrade>> {
        let row =
            sqlx::query_as::<_, ScoredTrade>(&format!("{SCORED_SELECT} WHERE s.trade_id = ?"))
                .bind(trade_id)
                .fetch_optional(self.pool())
                .await?;
        Ok(row)
    }

    /// Highest scores of trades made at or after `since`.
    pub async fn top_scores(&self, since: i64, limit: i64) -> Result<Vec<ScoredTrade>> {
        let rows = sqlx::query_as::<_, ScoredTrade>(&format!(
            "{SCORED_SELECT} WHERE t.timestamp >= ? ORDER BY s.total DESC, t.timestamp DESC LIMIT ?"
        ))
        .bind(since)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Most recent scored trades with a total of at least `min_total`.
    pub async fn recent_scores(&self, min_total: f64, limit: i64) -> Result<Vec<ScoredTrade>> {
        let rows = sqlx::query_as::<_, ScoredTrade>(&format!(
            "{SCORED_SELECT} WHERE s.total >= ? ORDER BY t.timestamp DESC, s.id DESC LIMIT ?"
        ))
        .bind(min_total)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Scores of a wallet, newest first.
    pub async fn wallet_scores(&self, wallet: &str, limit: i64) -> Result<Vec<ScoredTrade>> {
        let rows = sqlx::query_as::<_, ScoredTrade>(&format!(
            "{SCORED_SELECT} WHERE t.proxy_wallet = ? ORDER BY t.timestamp DESC LIMIT ?"
        ))
        .bind(wallet)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Distinct wallets that bought `token_id` within `[from, to]`, with the
    /// time of their first buy in that window.
    pub async fn buyers_in_window(
        &self,
        token_id: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT proxy_wallet, MIN(timestamp) FROM trades
             WHERE token_id = ? AND side = 'BUY' AND timestamp BETWEEN ? AND ?
             GROUP BY proxy_wallet",
        )
        .bind(token_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Trades that have no score yet (e.g. after a restart), oldest first.
    pub async fn unscored_trade_ids(&self, limit: i64) -> Result<Vec<i64>> {
        let ids = sqlx::query_scalar(
            "SELECT t.id FROM trades t LEFT JOIN scores s ON s.trade_id = t.id
             WHERE s.id IS NULL ORDER BY t.timestamp, t.id LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NewTrade;
    use smartmoney_core::Side;

    async fn add_trade(store: &Store, hash: &str, wallet: &str, side: Side, ts: i64) -> i64 {
        store
            .insert_trade(
                &NewTrade {
                    transaction_hash: hash.into(),
                    proxy_wallet: wallet.into(),
                    condition_id: "0xc".into(),
                    token_id: "tok".into(),
                    side,
                    outcome: Some("Yes".into()),
                    outcome_index: Some(0),
                    size: 10_000.0,
                    price: 0.3,
                    timestamp: ts,
                    title: Some("Q?".into()),
                    slug: None,
                    event_slug: None,
                    raw_json: None,
                },
                ts,
            )
            .await
            .unwrap()
            .unwrap()
    }

    fn score(trade_id: i64, total: f64) -> NewScore {
        NewScore {
            trade_id,
            proxy_wallet: "0xw".into(),
            condition_id: "0xc".into(),
            total,
            wallet_freshness: total,
            price_asymmetry: 0.0,
            relative_size: 0.0,
            concentration: 0.0,
            market_type: 0.0,
            cluster_bonus: 0.0,
            details_json: "{}".into(),
            scorer_version: 1,
            created_at: 1,
        }
    }

    #[tokio::test]
    async fn upsert_and_query_scores() {
        let store = Store::open_in_memory().await.unwrap();
        let t1 = add_trade(&store, "0x1", "0xw", Side::Buy, 100).await;
        let t2 = add_trade(&store, "0x2", "0xw", Side::Buy, 200).await;
        assert_eq!(store.unscored_trade_ids(10).await.unwrap(), vec![t1, t2]);

        let id1 = store.upsert_score(&score(t1, 40.0)).await.unwrap();
        let id1b = store.upsert_score(&score(t1, 80.0)).await.unwrap();
        assert_eq!(id1, id1b, "re-scoring keeps the row");
        store.upsert_score(&score(t2, 50.0)).await.unwrap();
        assert!(store.unscored_trade_ids(10).await.unwrap().is_empty());

        let top = store.top_scores(0, 10).await.unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].trade_id, t1);
        assert_eq!(top[0].total, 80.0);
        assert_eq!(store.top_scores(150, 10).await.unwrap().len(), 1);
        assert_eq!(store.recent_scores(60.0, 10).await.unwrap().len(), 1);
        assert_eq!(store.wallet_scores("0xw", 10).await.unwrap().len(), 2);
        let s = store.scored_trade(id1).await.unwrap().unwrap();
        assert_eq!(s.usdc_value, 3000.0);
        assert_eq!(
            store.score_for_trade(t2).await.unwrap().unwrap().total,
            50.0
        );
    }

    #[tokio::test]
    async fn buyers_in_window_groups_wallets() {
        let store = Store::open_in_memory().await.unwrap();
        add_trade(&store, "0x1", "0xa", Side::Buy, 100).await;
        add_trade(&store, "0x2", "0xa", Side::Buy, 150).await;
        add_trade(&store, "0x3", "0xb", Side::Buy, 160).await;
        add_trade(&store, "0x4", "0xc", Side::Sell, 170).await;
        add_trade(&store, "0x5", "0xd", Side::Buy, 900).await;
        let mut buyers = store.buyers_in_window("tok", 50, 200).await.unwrap();
        buyers.sort();
        assert_eq!(buyers, vec![("0xa".into(), 100), ("0xb".into(), 160)]);
    }
}
