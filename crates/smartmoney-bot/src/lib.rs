#![forbid(unsafe_code)]

//! Telegram alerts and allowlisted bot commands, straight against the
//! Telegram HTTP API (no framework). Analysis and paper trading only.

pub mod client;
pub mod format;
pub mod service;

pub use client::TelegramClient;
pub use service::Bot;
