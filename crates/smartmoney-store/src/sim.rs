//! Paper-trading persistence: accounts, positions, fills, equity.

use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SimAccount {
    pub id: i64,
    pub strategy: String,
    pub config_json: String,
    pub starting_balance: f64,
    pub cash: f64,
    pub created_at: i64,
}

/// A new paper position; the fill is stored alongside it.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSimPosition {
    pub account_id: i64,
    pub score_id: Option<i64>,
    pub condition_id: String,
    pub token_id: String,
    pub outcome_index: Option<i64>,
    /// 'auto' or 'manual'.
    pub origin: String,
    pub shares: f64,
    pub avg_price: f64,
    /// Total cash spent including fees.
    pub cost_usd: f64,
    pub fees_usd: f64,
    /// Price of the triggering trade or decision reference, if any.
    pub trigger_price: Option<f64>,
    pub decided_at: i64,
    pub filled_at: i64,
    pub snapshot_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SimPosition {
    pub id: i64,
    pub account_id: i64,
    pub condition_id: String,
    pub token_id: String,
    pub outcome_index: Option<i64>,
    pub origin: String,
    pub shares: f64,
    pub avg_price: f64,
    pub cost_usd: f64,
    pub fees_usd: f64,
    pub insider_price: Option<f64>,
    pub status: String,
    pub payout_usd: Option<f64>,
    pub realized_pnl: Option<f64>,
    pub opened_at: i64,
    pub settled_at: Option<i64>,
    pub title: Option<String>,
}

const POSITION_COLUMNS: &str = "p.id, p.account_id, p.condition_id, p.token_id,
    p.outcome_index, p.origin, p.shares, p.avg_price, p.cost_usd, p.fees_usd,
    p.insider_price, p.status, p.payout_usd, p.realized_pnl, p.opened_at,
    p.settled_at, m.question AS title";

impl Store {
    /// Fetch or create the account of a strategy.
    pub async fn ensure_sim_account(
        &self,
        strategy: &str,
        config_json: &str,
        starting_balance: f64,
        now: i64,
    ) -> Result<SimAccount> {
        sqlx::query(
            "INSERT INTO sim_accounts (strategy, config_json, starting_balance, cash, created_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (strategy) DO UPDATE SET config_json = excluded.config_json",
        )
        .bind(strategy)
        .bind(config_json)
        .bind(starting_balance)
        .bind(starting_balance)
        .bind(now)
        .execute(self.pool())
        .await?;
        let account = sqlx::query_as::<_, SimAccount>(
            "SELECT id, strategy, config_json, starting_balance, cash, created_at
             FROM sim_accounts WHERE strategy = ?",
        )
        .bind(strategy)
        .fetch_one(self.pool())
        .await?;
        Ok(account)
    }

    pub async fn sim_accounts(&self) -> Result<Vec<SimAccount>> {
        Ok(sqlx::query_as(
            "SELECT id, strategy, config_json, starting_balance, cash, created_at
             FROM sim_accounts ORDER BY strategy",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// Whether the account already holds any position in this market.
    pub async fn has_sim_position(&self, account_id: i64, condition_id: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sim_positions WHERE account_id = ? AND condition_id = ?",
        )
        .bind(account_id)
        .bind(condition_id)
        .fetch_one(self.pool())
        .await?;
        Ok(n > 0)
    }

    /// Open a position: inserts the position and its fill and debits the
    /// account's cash, all in one transaction. Fails if cash would go
    /// negative.
    pub async fn open_sim_position(&self, p: &NewSimPosition) -> Result<i64> {
        let mut tx = self.pool().begin().await?;
        let updated =
            sqlx::query("UPDATE sim_accounts SET cash = cash - ? WHERE id = ? AND cash >= ?")
                .bind(p.cost_usd)
                .bind(p.account_id)
                .bind(p.cost_usd)
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() == 0 {
            return Err(crate::StoreError::Constraint(
                "insufficient simulated cash".into(),
            ));
        }
        let position_id: i64 = sqlx::query_scalar(
            "INSERT INTO sim_positions (
                account_id, score_id, condition_id, token_id, outcome_index,
                origin, shares, avg_price, cost_usd, fees_usd, insider_price,
                status, opened_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?)
             RETURNING id",
        )
        .bind(p.account_id)
        .bind(p.score_id)
        .bind(&p.condition_id)
        .bind(&p.token_id)
        .bind(p.outcome_index)
        .bind(&p.origin)
        .bind(p.shares)
        .bind(p.avg_price)
        .bind(p.cost_usd)
        .bind(p.fees_usd)
        .bind(p.trigger_price)
        .bind(p.filled_at)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO sim_fills (position_id, shares, price, fee_usd, decided_at, filled_at, snapshot_id)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(position_id)
        .bind(p.shares)
        .bind(p.avg_price)
        .bind(p.fees_usd)
        .bind(p.decided_at)
        .bind(p.filled_at)
        .bind(p.snapshot_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(position_id)
    }

    /// Open positions in a market across all accounts.
    pub async fn open_sim_positions_by_condition(
        &self,
        condition_id: &str,
    ) -> Result<Vec<SimPosition>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {POSITION_COLUMNS} FROM sim_positions p
             LEFT JOIN markets m ON m.condition_id = p.condition_id
             WHERE p.condition_id = ? AND p.status = 'open'",
        ))
        .bind(condition_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Settle one open position and credit the proceeds, in one transaction.
    pub async fn settle_sim_position(
        &self,
        position_id: i64,
        payout_usd: f64,
        realized_pnl: f64,
        settled_at: i64,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query(
            "UPDATE sim_positions
             SET status = 'settled', payout_usd = ?, realized_pnl = ?, settled_at = ?
             WHERE id = ? AND status = 'open'",
        )
        .bind(payout_usd)
        .bind(realized_pnl)
        .bind(settled_at)
        .bind(position_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 1 {
            sqlx::query(
                "UPDATE sim_accounts SET cash = cash + ?
                 WHERE id = (SELECT account_id FROM sim_positions WHERE id = ?)",
            )
            .bind(payout_usd)
            .bind(position_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Latest positions of one account (open first, then recently settled).
    pub async fn sim_positions(&self, account_id: i64, limit: i64) -> Result<Vec<SimPosition>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {POSITION_COLUMNS} FROM sim_positions p
             LEFT JOIN markets m ON m.condition_id = p.condition_id
             WHERE p.account_id = ?
             ORDER BY p.status = 'open' DESC, p.opened_at DESC LIMIT ?",
        ))
        .bind(account_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?)
    }

    /// All open positions (for equity marks).
    pub async fn open_sim_positions(&self) -> Result<Vec<SimPosition>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {POSITION_COLUMNS} FROM sim_positions p
             LEFT JOIN markets m ON m.condition_id = p.condition_id
             WHERE p.status = 'open'",
        ))
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn insert_sim_equity(&self, account_id: i64, ts: i64, equity: f64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sim_equity (account_id, ts, equity) VALUES (?, ?, ?)
             ON CONFLICT (account_id, ts) DO UPDATE SET equity = excluded.equity",
        )
        .bind(account_id)
        .bind(ts)
        .bind(equity)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Equity series of one account, oldest first.
    pub async fn sim_equity(&self, account_id: i64, limit: i64) -> Result<Vec<(i64, f64)>> {
        let rows: Vec<(i64, f64)> = sqlx::query_as(
            "SELECT ts, equity FROM (
                 SELECT ts, equity FROM sim_equity WHERE account_id = ?
                 ORDER BY ts DESC LIMIT ?
             ) ORDER BY ts ASC",
        )
        .bind(account_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(account_id: i64) -> NewSimPosition {
        NewSimPosition {
            account_id,
            score_id: None,
            condition_id: "0xc1".into(),
            token_id: "t1".into(),
            outcome_index: Some(0),
            origin: "auto".into(),
            shares: 103.0,
            avg_price: 0.96,
            cost_usd: 99.0,
            fees_usd: 0.2,
            trigger_price: Some(0.955),
            decided_at: 100,
            filled_at: 130,
            snapshot_id: None,
        }
    }

    #[tokio::test]
    async fn account_positions_and_settlement() {
        let store = Store::open_in_memory().await.unwrap();
        let acc = store
            .ensure_sim_account("favorites98", "{}", 5000.0, 50)
            .await
            .unwrap();
        assert_eq!(acc.cash, 5000.0);
        // Idempotent: a second call keeps the cash.
        let again = store
            .ensure_sim_account("favorites98", "{\"v\":2}", 5000.0, 60)
            .await
            .unwrap();
        assert_eq!(again.id, acc.id);
        assert_eq!(again.cash, 5000.0);
        assert_eq!(again.config_json, "{\"v\":2}");

        assert!(!store.has_sim_position(acc.id, "0xc1").await.unwrap());
        let id = store.open_sim_position(&pos(acc.id)).await.unwrap();
        assert!(store.has_sim_position(acc.id, "0xc1").await.unwrap());
        let after = store.sim_accounts().await.unwrap();
        assert!((after[0].cash - 4901.0).abs() < 1e-9);

        // Settlement: 103 shares pay out $1 each.
        store
            .settle_sim_position(id, 103.0, 103.0 - 99.0, 999)
            .await
            .unwrap();
        let after = store.sim_accounts().await.unwrap();
        assert!((after[0].cash - 5004.0).abs() < 1e-9);
        let rows = store.sim_positions(acc.id, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "settled");
        assert!((rows[0].realized_pnl.unwrap() - 4.0).abs() < 1e-9);
        // Settling twice changes nothing.
        store
            .settle_sim_position(id, 103.0, 4.0, 1000)
            .await
            .unwrap();
        let after = store.sim_accounts().await.unwrap();
        assert!((after[0].cash - 5004.0).abs() < 1e-9);

        assert!(store.open_sim_positions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_overdraft_and_tracks_equity() {
        let store = Store::open_in_memory().await.unwrap();
        let acc = store
            .ensure_sim_account("anchor2", "{}", 50.0, 1)
            .await
            .unwrap();
        let err = store.open_sim_position(&pos(acc.id)).await;
        assert!(err.is_err(), "cost 99 exceeds cash 50");
        assert!(!store.has_sim_position(acc.id, "0xc1").await.unwrap());

        store.insert_sim_equity(acc.id, 10, 50.0).await.unwrap();
        store.insert_sim_equity(acc.id, 20, 51.0).await.unwrap();
        store.insert_sim_equity(acc.id, 20, 52.0).await.unwrap();
        assert_eq!(
            store.sim_equity(acc.id, 10).await.unwrap(),
            vec![(10, 50.0), (20, 52.0)]
        );
    }
}
