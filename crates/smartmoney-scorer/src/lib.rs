#![forbid(unsafe_code)]

//! Insider score (0-100) per trade with a stored breakdown.

pub mod model;
pub mod service;

pub use model::{score, ScoreBreakdown, ScoreInput, TradeFacts, SCORER_VERSION};
pub use service::{effective_threshold, Scorer, ScorerEvent, STATE_ALERT_THRESHOLD};
