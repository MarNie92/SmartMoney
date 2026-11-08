# SmartMoney

SmartMoney watches public Polymarket activity for conspicuous wallets
(possible insiders / "smart money"), sends Telegram alerts and tests the
signals with paper trading. **It never places real trades or submits
orders.**

## Status

| Milestone | Content | Status |
|---|---|---|
| 1 | Repo setup, workspace, config, SQLite schema, docs | done |
| 2 | Collector + storage, live API test | open |
| 3 | Scorer with unit tests | open |
| 4 | Backtest CLI | open |
| 5 | Telegram bot | open |
| 6 | Paper trading incl. inline buttons and stats | open |
| 7 | Web dashboard | open |
| 8 | Deployment docs, systemd and backup scripts | open |
| 9 | Optional: Tauri wrapper | open |

## Requirements

- Rust (stable, edition 2021, MSRV 1.85) with `rustfmt` and `clippy`
- Windows: Visual Studio Build Tools with the C++ workload (MSVC linker;
  SQLite is compiled from source)
- Linux / Oracle Cloud Ampere (aarch64): `build-essential`, `pkg-config`

## Setup

```sh
cp .env.example .env                       # PowerShell: Copy-Item .env.example .env
cp config.example.toml config.local.toml   # adjust as needed
cargo build --workspace
```

### Configuration

Non-secret settings live in a TOML file:

1. `$SMARTMONEY_CONFIG`, if set
2. otherwise `config.local.toml`, if present
3. otherwise built-in defaults (identical to `config.example.toml`)

Unknown keys are rejected so typos surface immediately.

Secrets come **only** from environment variables (or a local `.env`, which
is ignored by Git). Putting a token in the TOML file is a parse error.

| Variable | Purpose |
|---|---|
| `SMARTMONEY_TELEGRAM_BOT_TOKEN` | Telegram bot token (secret) |
| `SMARTMONEY_TELEGRAM_ALLOWED_CHAT_IDS` | Comma-separated chat IDs allowed to use the bot |
| `SMARTMONEY_DASHBOARD_TOKEN` | Dashboard access token (secret) |
| `SMARTMONEY_DASHBOARD_BIND` | Override dashboard bind address |
| `SMARTMONEY_DATABASE_PATH` | Override SQLite path |
| `SMARTMONEY_CONFIG` | Path to the TOML config |
| `RUST_LOG` | Log filter, e.g. `info,smartmoney_collector=debug` |

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p smartmoney-daemon          # start the daemon
cargo run -p smartmoney-backtest        # backtest CLI (milestone 4)
```

Currently the daemon loads the config, opens/migrates the database and waits
for Ctrl+C. Services are added milestone by milestone.

## Workspace

| Crate | Purpose |
|---|---|
| `smartmoney-core` | Shared types, configuration (TOML + env) |
| `smartmoney-collector` | Polls Polymarket APIs, rate limiting with backoff |
| `smartmoney-store` | SQLite via sqlx, migrations |
| `smartmoney-scorer` | Insider score per trade / wallet |
| `smartmoney-bot` | Telegram bot (teloxide) |
| `smartmoney-web` | axum server with mobile-first dashboard |
| `smartmoney-sim` | Paper trading |
| `smartmoney-backtest` | CLI replaying resolved markets |
| `smartmoney-daemon` (apps/) | Runs collector, scorer, sim, bot and web together |

## Data sources

Verified against docs.polymarket.com on 2026-11-08:

- Gamma API `https://gamma-api.polymarket.com`: markets, events
- Data API `https://data-api.polymarket.com`: **v2 routes** (`/v2/trades`,
  `/v2/activity`, `/v2/positions`, `/v2/holders`, `/v2/user-stats`,
  `/v2/resolutions`). The v1 routes were retired on 2026-11-02.
- CLOB API `https://clob.polymarket.com`: `/book`, `/midpoint`,
  `/prices-history`

## Database

SQLite, schema in `crates/smartmoney-store/migrations`. Migrations run
automatically at startup. The database file is never committed. Main tables:
`markets`, `trades`, `wallets`, `wallet_positions`, `scores` (with breakdown),
`alerts`, `watchlist`, `orderbook_snapshots`, `price_history`,
`sim_accounts`, `sim_positions`, `sim_fills`, `sim_equity`, `backtest_runs`,
`kv_state`.
