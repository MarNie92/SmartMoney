#![forbid(unsafe_code)]

//! Polymarket data collection and enrichment.
//!
//! Read-only access to public APIs. Nothing here places orders.

pub mod api;
pub mod client;
pub mod ratelimit;
pub mod service;

pub use client::{ApiError, PolymarketClient};
pub use service::{Collector, CollectorError, CollectorEvent, PollStats};
