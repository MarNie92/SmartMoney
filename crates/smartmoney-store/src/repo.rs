//! Repository methods for collector data.

use crate::records::{
    BookSnapshot, MarketRecord, MarketRow, NewTrade, PositionRecord, Resolution, TradeRow,
    WalletRecord,
};
use crate::{Result, Store};
use smartmoney_core::MarketCategory;
use sqlx::Row;

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serializing plain data cannot fail")
}

fn from_json<T: serde::de::DeserializeOwned + Default>(text: Option<String>) -> T {
    text.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

impl Store {
    /// Insert a trade. Returns the new row id, or `None` if it already existed.
    pub async fn insert_trade(&self, trade: &NewTrade, ingested_at: i64) -> Result<Option<i64>> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO trades (
                transaction_hash, proxy_wallet, condition_id, token_id, side, outcome,
                outcome_index, size, price, usdc_value, timestamp, title, slug, event_slug,
                raw_json, ingested_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&trade.transaction_hash)
        .bind(&trade.proxy_wallet)
        .bind(&trade.condition_id)
        .bind(&trade.token_id)
        .bind(trade.side.as_str())
        .bind(&trade.outcome)
        .bind(trade.outcome_index)
        .bind(trade.size)
        .bind(trade.price)
        .bind(trade.usdc_value())
        .bind(trade.timestamp)
        .bind(&trade.title)
        .bind(&trade.slug)
        .bind(&trade.event_slug)
        .bind(&trade.raw_json)
        .bind(ingested_at)
        .execute(self.pool())
        .await?;
        Ok((result.rows_affected() == 1).then(|| result.last_insert_rowid()))
    }

    pub async fn trade(&self, id: i64) -> Result<Option<TradeRow>> {
        let row = sqlx::query_as::<_, TradeRow>(&format!("{TRADE_SELECT} WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row)
    }

    /// Most recent trades, newest first.
    pub async fn recent_trades(&self, limit: i64) -> Result<Vec<TradeRow>> {
        let rows = sqlx::query_as::<_, TradeRow>(&format!(
            "{TRADE_SELECT} ORDER BY timestamp DESC, id DESC LIMIT ?"
        ))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    pub async fn count_trades(&self) -> Result<i64> {
        let n = sqlx::query_scalar("SELECT COUNT(*) FROM trades")
            .fetch_one(self.pool())
            .await?;
        Ok(n)
    }

    /// Insert or update market data without touching resolution fields.
    pub async fn upsert_market(&self, m: &MarketRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO markets (
                condition_id, gamma_id, question, slug, event_slug, category, gamma_category,
                tags_json, volume_usd, volume_24h_usd, end_date, closed, outcomes_json,
                outcome_prices_json, clob_token_ids_json, neg_risk, resolution_status,
                raw_json, fetched_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (condition_id) DO UPDATE SET
                gamma_id = excluded.gamma_id,
                question = excluded.question,
                slug = excluded.slug,
                event_slug = COALESCE(excluded.event_slug, markets.event_slug),
                category = excluded.category,
                gamma_category = excluded.gamma_category,
                tags_json = excluded.tags_json,
                volume_usd = excluded.volume_usd,
                volume_24h_usd = excluded.volume_24h_usd,
                end_date = excluded.end_date,
                closed = excluded.closed,
                outcomes_json = excluded.outcomes_json,
                outcome_prices_json = excluded.outcome_prices_json,
                clob_token_ids_json = excluded.clob_token_ids_json,
                neg_risk = excluded.neg_risk,
                resolution_status = excluded.resolution_status,
                raw_json = excluded.raw_json,
                fetched_at = excluded.fetched_at",
        )
        .bind(&m.condition_id)
        .bind(&m.gamma_id)
        .bind(&m.question)
        .bind(&m.slug)
        .bind(&m.event_slug)
        .bind(m.category.as_str())
        .bind(&m.gamma_category)
        .bind(to_json(&m.tags))
        .bind(m.volume_usd)
        .bind(m.volume_24h_usd)
        .bind(m.end_date)
        .bind(m.closed)
        .bind(to_json(&m.outcomes))
        .bind(to_json(&m.outcome_prices))
        .bind(to_json(&m.clob_token_ids))
        .bind(m.neg_risk)
        .bind(&m.resolution_status)
        .bind(&m.raw_json)
        .bind(m.fetched_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn market(&self, condition_id: &str) -> Result<Option<MarketRow>> {
        let row = sqlx::query(
            "SELECT condition_id, gamma_id, question, slug, event_slug, category,
                    gamma_category, tags_json, volume_usd, volume_24h_usd, end_date, closed,
                    outcomes_json, outcome_prices_json, clob_token_ids_json, neg_risk,
                    resolution_status, raw_json, fetched_at, payouts_json,
                    resolved_outcome_index, resolved_at
             FROM markets WHERE condition_id = ?",
        )
        .bind(condition_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let category: Option<String> = row.try_get("category")?;
        let payouts: Option<String> = row.try_get("payouts_json")?;
        Ok(Some(MarketRow {
            market: MarketRecord {
                condition_id: row.try_get("condition_id")?,
                gamma_id: row.try_get("gamma_id")?,
                question: row.try_get("question")?,
                slug: row.try_get("slug")?,
                event_slug: row.try_get("event_slug")?,
                category: category
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(MarketCategory::Other),
                gamma_category: row.try_get("gamma_category")?,
                tags: from_json(row.try_get("tags_json")?),
                volume_usd: row.try_get("volume_usd")?,
                volume_24h_usd: row.try_get("volume_24h_usd")?,
                end_date: row.try_get("end_date")?,
                closed: row.try_get("closed")?,
                outcomes: from_json(row.try_get("outcomes_json")?),
                outcome_prices: from_json(row.try_get("outcome_prices_json")?),
                clob_token_ids: from_json(row.try_get("clob_token_ids_json")?),
                neg_risk: row.try_get("neg_risk")?,
                resolution_status: row.try_get("resolution_status")?,
                raw_json: row.try_get("raw_json")?,
                fetched_at: row.try_get("fetched_at")?,
            },
            payouts: payouts.and_then(|p| serde_json::from_str(&p).ok()),
            resolved_outcome_index: row.try_get("resolved_outcome_index")?,
            resolved_at: row.try_get("resolved_at")?,
        }))
    }

    pub async fn market_fetched_at(&self, condition_id: &str) -> Result<Option<i64>> {
        let ts = sqlx::query_scalar("SELECT fetched_at FROM markets WHERE condition_id = ?")
            .bind(condition_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(ts)
    }

    pub async fn set_market_resolution(&self, condition_id: &str, r: &Resolution) -> Result<()> {
        sqlx::query(
            "UPDATE markets SET resolution_status = ?, payouts_json = ?,
                    resolved_outcome_index = ?, resolved_at = ?, closed = 1
             WHERE condition_id = ?",
        )
        .bind(&r.status)
        .bind(to_json(&r.payouts))
        .bind(r.winning_outcome_index())
        .bind(r.resolved_at)
        .bind(condition_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Markets without a recorded resolution whose end date has passed (or
    /// that Gamma reports as closed). Oldest check first.
    pub async fn markets_awaiting_resolution(&self, now: i64, limit: i64) -> Result<Vec<String>> {
        let ids = sqlx::query_scalar(
            "SELECT condition_id FROM markets
             WHERE payouts_json IS NULL AND (closed = 1 OR end_date <= ?)
             ORDER BY fetched_at ASC LIMIT ?",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(ids)
    }

    pub async fn upsert_wallet(&self, w: &WalletRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO wallets (
                address, name, pseudonym, join_date, first_activity_at, markets_traded,
                trade_count, total_positions_usd, largest_position_usd, raw_stats_json,
                enriched_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (address) DO UPDATE SET
                name = COALESCE(excluded.name, wallets.name),
                pseudonym = COALESCE(excluded.pseudonym, wallets.pseudonym),
                join_date = COALESCE(excluded.join_date, wallets.join_date),
                first_activity_at = COALESCE(excluded.first_activity_at, wallets.first_activity_at),
                markets_traded = excluded.markets_traded,
                trade_count = excluded.trade_count,
                total_positions_usd = excluded.total_positions_usd,
                largest_position_usd = excluded.largest_position_usd,
                raw_stats_json = excluded.raw_stats_json,
                enriched_at = excluded.enriched_at",
        )
        .bind(&w.address)
        .bind(&w.name)
        .bind(&w.pseudonym)
        .bind(w.join_date)
        .bind(w.first_activity_at)
        .bind(w.markets_traded)
        .bind(w.trade_count)
        .bind(w.total_positions_usd)
        .bind(w.largest_position_usd)
        .bind(&w.raw_stats_json)
        .bind(w.enriched_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn wallet(&self, address: &str) -> Result<Option<WalletRecord>> {
        let row = sqlx::query(
            "SELECT address, name, pseudonym, join_date, first_activity_at, markets_traded,
                    trade_count, total_positions_usd, largest_position_usd, raw_stats_json,
                    enriched_at
             FROM wallets WHERE address = ?",
        )
        .bind(address)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(WalletRecord {
            address: row.try_get("address")?,
            name: row.try_get("name")?,
            pseudonym: row.try_get("pseudonym")?,
            join_date: row.try_get("join_date")?,
            first_activity_at: row.try_get("first_activity_at")?,
            markets_traded: row.try_get("markets_traded")?,
            trade_count: row.try_get("trade_count")?,
            total_positions_usd: row.try_get("total_positions_usd")?,
            largest_position_usd: row.try_get("largest_position_usd")?,
            raw_stats_json: row.try_get("raw_stats_json")?,
            enriched_at: row.try_get::<Option<i64>, _>("enriched_at")?.unwrap_or(0),
        }))
    }

    pub async fn wallet_enriched_at(&self, address: &str) -> Result<Option<i64>> {
        let ts: Option<Option<i64>> =
            sqlx::query_scalar("SELECT enriched_at FROM wallets WHERE address = ?")
                .bind(address)
                .fetch_optional(self.pool())
                .await?;
        Ok(ts.flatten())
    }

    /// Replace all stored positions of a wallet atomically.
    pub async fn replace_wallet_positions(
        &self,
        address: &str,
        positions: &[PositionRecord],
        fetched_at: i64,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM wallet_positions WHERE address = ?")
            .bind(address)
            .execute(&mut *tx)
            .await?;
        for p in positions {
            sqlx::query(
                "INSERT OR REPLACE INTO wallet_positions (
                    address, token_id, condition_id, current_size, current_value, total_cost,
                    fetched_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(address)
            .bind(&p.token_id)
            .bind(&p.condition_id)
            .bind(p.current_size)
            .bind(p.current_value)
            .bind(p.total_cost)
            .bind(fetched_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn wallet_positions(&self, address: &str) -> Result<Vec<PositionRecord>> {
        let rows = sqlx::query(
            "SELECT token_id, condition_id, current_size, current_value, total_cost
             FROM wallet_positions WHERE address = ? ORDER BY current_value DESC",
        )
        .bind(address)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PositionRecord {
                    token_id: row.try_get("token_id")?,
                    condition_id: row.try_get("condition_id")?,
                    current_size: row.try_get("current_size")?,
                    current_value: row.try_get("current_value")?,
                    total_cost: row.try_get("total_cost")?,
                })
            })
            .collect()
    }

    /// Store an order book snapshot; returns its id.
    pub async fn insert_book_snapshot(&self, book: &BookSnapshot) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO orderbook_snapshots (token_id, taken_at, best_bid, best_ask, bids_json, asks_json)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&book.token_id)
        .bind(book.taken_at)
        .bind(book.best_bid())
        .bind(book.best_ask())
        .bind(to_json(&book.bids))
        .bind(to_json(&book.asks))
        .execute(self.pool())
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// Latest snapshot for a token taken at or before `at`.
    pub async fn book_snapshot_at(&self, token_id: &str, at: i64) -> Result<Option<BookSnapshot>> {
        let row = sqlx::query(
            "SELECT token_id, taken_at, bids_json, asks_json FROM orderbook_snapshots
             WHERE token_id = ? AND taken_at <= ? ORDER BY taken_at DESC, id DESC LIMIT 1",
        )
        .bind(token_id)
        .bind(at)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(BookSnapshot {
            token_id: row.try_get("token_id")?,
            taken_at: row.try_get("taken_at")?,
            bids: from_json(row.try_get("bids_json")?),
            asks: from_json(row.try_get("asks_json")?),
        }))
    }

    pub async fn insert_price_history(&self, token_id: &str, points: &[(i64, f64)]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        for (ts, price) in points {
            sqlx::query(
                "INSERT OR REPLACE INTO price_history (token_id, ts, price) VALUES (?, ?, ?)",
            )
            .bind(token_id)
            .bind(ts)
            .bind(price)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Price points for a token within `[from, to]`, ascending.
    pub async fn price_history(
        &self,
        token_id: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<(i64, f64)>> {
        let rows = sqlx::query_as::<_, (i64, f64)>(
            "SELECT ts, price FROM price_history WHERE token_id = ? AND ts BETWEEN ? AND ? ORDER BY ts",
        )
        .bind(token_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }
}

const TRADE_SELECT: &str = "SELECT id, transaction_hash, proxy_wallet, condition_id, token_id, side, outcome,
    outcome_index, size, price, usdc_value, timestamp, title, slug, event_slug, ingested_at FROM trades";

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::Side;

    fn trade(hash: &str, ts: i64) -> NewTrade {
        NewTrade {
            transaction_hash: hash.into(),
            proxy_wallet: "0xabc".into(),
            condition_id: "0xcond".into(),
            token_id: "123".into(),
            side: Side::Buy,
            outcome: Some("Yes".into()),
            outcome_index: Some(0),
            size: 10_000.0,
            price: 0.25,
            timestamp: ts,
            title: Some("Will X happen?".into()),
            slug: None,
            event_slug: None,
            raw_json: None,
        }
    }

    fn market(cid: &str) -> MarketRecord {
        MarketRecord {
            condition_id: cid.into(),
            gamma_id: Some("1".into()),
            question: Some("Will X happen?".into()),
            slug: Some("will-x".into()),
            event_slug: Some("x".into()),
            category: MarketCategory::Personnel,
            gamma_category: None,
            tags: vec!["politics".into()],
            volume_usd: Some(1000.0),
            volume_24h_usd: Some(100.0),
            end_date: Some(500),
            closed: false,
            outcomes: vec!["Yes".into(), "No".into()],
            outcome_prices: vec![0.3, 0.7],
            clob_token_ids: vec!["1".into(), "2".into()],
            neg_risk: Some(false),
            resolution_status: None,
            raw_json: None,
            fetched_at: 10,
        }
    }

    #[tokio::test]
    async fn trades_are_deduplicated() {
        let store = Store::open_in_memory().await.unwrap();
        let id = store.insert_trade(&trade("0x1", 100), 1).await.unwrap();
        assert!(id.is_some());
        assert_eq!(
            store.insert_trade(&trade("0x1", 100), 2).await.unwrap(),
            None
        );
        store.insert_trade(&trade("0x2", 200), 2).await.unwrap();
        assert_eq!(store.count_trades().await.unwrap(), 2);

        let row = store.trade(id.unwrap()).await.unwrap().unwrap();
        assert_eq!(row.usdc_value, 2500.0);
        assert_eq!(row.side, "BUY");
        let recent = store.recent_trades(10).await.unwrap();
        assert_eq!(recent[0].transaction_hash, "0x2");
    }

    #[tokio::test]
    async fn market_upsert_keeps_resolution() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert_market(&market("0xm")).await.unwrap();
        assert_eq!(
            store.markets_awaiting_resolution(1000, 10).await.unwrap(),
            vec!["0xm".to_string()]
        );
        assert!(store
            .markets_awaiting_resolution(100, 10)
            .await
            .unwrap()
            .is_empty());

        store
            .set_market_resolution(
                "0xm",
                &Resolution {
                    status: "resolved".into(),
                    payouts: vec![0.0, 1.0],
                    resolved_at: Some(600),
                },
            )
            .await
            .unwrap();
        // A later Gamma refresh must not wipe the resolution.
        let mut refreshed = market("0xm");
        refreshed.fetched_at = 20;
        store.upsert_market(&refreshed).await.unwrap();

        let row = store.market("0xm").await.unwrap().unwrap();
        assert_eq!(row.resolved_outcome_index, Some(1));
        assert_eq!(row.payouts, Some(vec![0.0, 1.0]));
        assert_eq!(row.market.category, MarketCategory::Personnel);
        assert_eq!(row.market.outcome_prices, vec![0.3, 0.7]);
        assert_eq!(row.market.fetched_at, 20);
        assert!(store
            .markets_awaiting_resolution(1000, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn wallet_and_positions() {
        let store = Store::open_in_memory().await.unwrap();
        let w = WalletRecord {
            address: "0xw".into(),
            join_date: Some(100),
            markets_traded: Some(2),
            enriched_at: 50,
            ..Default::default()
        };
        store.upsert_wallet(&w).await.unwrap();
        // Later enrichment without join_date keeps the known value.
        store
            .upsert_wallet(&WalletRecord {
                join_date: None,
                markets_traded: Some(3),
                enriched_at: 60,
                ..w.clone()
            })
            .await
            .unwrap();
        let stored = store.wallet("0xw").await.unwrap().unwrap();
        assert_eq!(stored.join_date, Some(100));
        assert_eq!(stored.markets_traded, Some(3));
        assert_eq!(store.wallet_enriched_at("0xw").await.unwrap(), Some(60));

        let pos = |t: &str, v: f64| PositionRecord {
            token_id: t.into(),
            condition_id: "0xc".into(),
            current_size: Some(1.0),
            current_value: Some(v),
            total_cost: Some(v),
        };
        store
            .replace_wallet_positions("0xw", &[pos("a", 1.0), pos("b", 5.0)], 60)
            .await
            .unwrap();
        store
            .replace_wallet_positions("0xw", &[pos("c", 3.0)], 70)
            .await
            .unwrap();
        let positions = store.wallet_positions("0xw").await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].token_id, "c");
    }

    #[tokio::test]
    async fn book_snapshots_and_history() {
        let store = Store::open_in_memory().await.unwrap();
        let book = BookSnapshot {
            token_id: "t".into(),
            taken_at: 100,
            bids: vec![(0.4, 10.0), (0.39, 5.0)],
            asks: vec![(0.42, 7.0)],
        };
        store.insert_book_snapshot(&book).await.unwrap();
        assert_eq!(store.book_snapshot_at("t", 99).await.unwrap(), None);
        assert_eq!(store.book_snapshot_at("t", 150).await.unwrap(), Some(book));

        store
            .insert_price_history("t", &[(1, 0.1), (2, 0.2), (3, 0.3)])
            .await
            .unwrap();
        assert_eq!(
            store.price_history("t", 2, 3).await.unwrap(),
            vec![(2, 0.2), (3, 0.3)]
        );
    }
}
