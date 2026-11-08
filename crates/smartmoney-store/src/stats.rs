//! Small aggregate queries for status displays.

use crate::{Result, Store};

impl Store {
    pub async fn count_trades_since(&self, since: i64) -> Result<i64> {
        let n = sqlx::query_scalar("SELECT COUNT(*) FROM trades WHERE timestamp >= ?")
            .bind(since)
            .fetch_one(self.pool())
            .await?;
        Ok(n)
    }

    /// Scored trades made at or after `since` with a total of at least `min_total`.
    pub async fn count_scores_since(&self, since: i64, min_total: f64) -> Result<i64> {
        let n = sqlx::query_scalar(
            "SELECT COUNT(*) FROM scores s JOIN trades t ON t.id = s.trade_id
             WHERE t.timestamp >= ? AND s.total >= ?",
        )
        .bind(since)
        .bind(min_total)
        .fetch_one(self.pool())
        .await?;
        Ok(n)
    }

    /// When a `kv_state` key was last written.
    pub async fn state_updated_at(&self, key: &str) -> Result<Option<i64>> {
        let ts = sqlx::query_scalar("SELECT updated_at FROM kv_state WHERE key = ?")
            .bind(key)
            .fetch_optional(self.pool())
            .await?;
        Ok(ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_counts_and_state_time() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(store.count_trades_since(0).await.unwrap(), 0);
        assert_eq!(store.count_scores_since(0, 0.0).await.unwrap(), 0);
        assert_eq!(store.state_updated_at("k").await.unwrap(), None);
        store.set_state("k", "v", 42).await.unwrap();
        assert_eq!(store.state_updated_at("k").await.unwrap(), Some(42));
    }
}
