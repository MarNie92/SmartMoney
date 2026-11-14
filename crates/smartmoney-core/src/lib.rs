//! Shared types and configuration for SmartMoney.
//!
//! SmartMoney only analyses public Polymarket data, sends signals and runs
//! paper trading. Nothing in this workspace submits real orders.

pub mod classify;
pub mod config;
pub mod fees;
pub mod secret;
pub mod types;

pub use classify::classify_market;
pub use config::{Config, ConfigError, ScoreMode};
pub use secret::Secret;
pub use types::{MarketCategory, Side};
