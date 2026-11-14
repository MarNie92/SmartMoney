#![forbid(unsafe_code)]

//! Paper trading for SmartMoney. Simulated fills against the live order
//! book only; this crate never places real orders and holds no keys.

pub mod anchor;
pub mod engine;
pub mod favorites;
pub mod service;

pub use service::{SimEvent, SimService};
