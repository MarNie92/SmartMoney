//! Shared types and configuration for SmartMoney.
//!
//! SmartMoney only analyses public Polymarket data, sends signals and runs
//! paper trading. Nothing in this workspace submits real orders.

pub mod config;
pub mod secret;
pub mod types;

pub use config::{Config, ConfigError};
pub use secret::Secret;
pub use types::{MarketCategory, Side};
