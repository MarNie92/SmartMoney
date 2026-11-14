//! SQLite persistence for SmartMoney.

pub mod records;
mod repo;
pub mod scores;
pub mod sim;
mod stats;

pub use records::*;
pub use scores::{NewScore, ScoredTrade};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub use sqlx;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("constraint violated: {0}")]
    Constraint(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Handle to the SmartMoney database.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if needed) the database file and run pending migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).await
    }

    /// In-memory database for tests.
    pub async fn open_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        // A single connection: every in-memory connection is its own database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).await
    }

    async fn from_pool(pool: SqlitePool) -> Result<Self> {
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Read a runtime setting / cursor.
    pub async fn get_state(&self, key: &str) -> Result<Option<String>> {
        let value = sqlx::query_scalar::<_, String>("SELECT value FROM kv_state WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(value)
    }

    /// Write a runtime setting / cursor.
    pub async fn set_state(&self, key: &str, value: &str, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_create_all_tables() {
        let store = Store::open_in_memory().await.unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_%' ESCAPE '\\' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        for expected in [
            "alerts",
            "backtest_runs",
            "kv_state",
            "markets",
            "orderbook_snapshots",
            "price_history",
            "scores",
            "sim_accounts",
            "sim_equity",
            "sim_fills",
            "sim_positions",
            "trades",
            "wallet_positions",
            "wallets",
            "watchlist",
        ] {
            assert!(tables.iter().any(|t| t == expected), "missing {expected}");
        }
    }

    #[tokio::test]
    async fn kv_state_upserts() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(store.get_state("threshold").await.unwrap(), None);
        store.set_state("threshold", "60", 1).await.unwrap();
        store.set_state("threshold", "70", 2).await.unwrap();
        assert_eq!(
            store.get_state("threshold").await.unwrap().as_deref(),
            Some("70")
        );
    }

    #[tokio::test]
    async fn open_file_database_twice_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("smartmoney-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        {
            let store = Store::open(&path).await.unwrap();
            store.set_state("k", "v", 1).await.unwrap();
            store.pool().close().await;
        }
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.get_state("k").await.unwrap().as_deref(), Some("v"));
        store.pool().close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
