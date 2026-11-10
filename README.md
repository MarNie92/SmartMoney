# SmartMoney

SmartMoney watches public Polymarket activity for conspicuous wallets
(possible insiders / "smart money"), sends Telegram alerts and tests the
signals with paper trading. **It never places real trades or submits
orders.**

## Status

See `ROADMAP.md` for the current plan (insider signals plus mirroring of
proven wallets, Telegram, paper trading per strategy).

| Milestone | Content | Status |
|---|---|---|
| 1 | Repo setup, workspace, config, SQLite schema, docs | done |
| 2 | Collector + storage, live API test | done |
| 3 | Scorer with unit tests | done |
| 4 | Backtest CLI | done |
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

The daemon currently runs the collector: every `poll_interval_seconds` it
fetches trades of at least `minimum_trade_usd` from `/v2/trades`, stores them,
enriches wallet (age, markets traded, positions) and market data (Gamma, with
a derived category), snapshots the order book of the traded token, and every
10 minutes records resolutions of ended markets. Each new trade is scored
(0-100) by the scorer; scores at or above `scoring.alert_threshold` are
logged as `ALERT` (Telegram follows in milestone 5).

## Dashboard

The daemon serves a mobile-first dashboard (German UI) at
`dashboard.bind_address` (default `http://127.0.0.1:8080`):

- **Feed**: latest scored trades, auto-refreshing every 20 s, filter by
  minimum score, alerts highlighted. Each row shows a breakdown strip: one
  segment per score component, width = weight, fill = points. Tap a row for
  the plain-language reasons and links.
- **Wallet** (`/wallet/<address>`): age, markets traded, positions and all
  scored trades of the wallet.

Access control: without `SMARTMONEY_DASHBOARD_TOKEN` the dashboard only binds
to loopback addresses (the daemon refuses to start otherwise). With a token,
open `http://<host>:8080/?token=<token>` once; it sets an HttpOnly cookie.
Use a token of letters and digits only.

Live API tests (network, ignored by default):

```sh
cargo test -p smartmoney-collector --test live_api -- --ignored --nocapture
```

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

## Scoring

Each trade gets a 0-100 insider score (capped), stored with a full breakdown
in `scores.details_json`. Only BUY trades are scored; SELLs get 0. All
thresholds and weights live in `[scoring]` (see `config.example.toml`).

| Component | Max | Full points | Fades to 0 at |
|---|---|---|---|
| Wallet freshness | 25 | younger than 7 days **or** fewer than 5 markets traded | 30 days / 20 markets |
| Price asymmetry | 25 | entry price <= 5 % | 20 % |
| Relative size | 20 | trade >= 10 % of the market's 24h volume | 0 % |
| Concentration | 15 | >= 80 % of the wallet's position value in this market | 20 % |
| Market type | 15 | factor per category: personnel/corporate/legal 1.0, politics 0.6, other 0.5, sports 0.1, crypto prices 0 | |
| Cluster bonus | +10 | >= 2 fresh wallets bought the same outcome within 30 min | |

Wallet age is measured at the trade time. The market category is derived
from Gamma tags and the question text (`smartmoney_core::classify_market`).
The cluster bonus only sees trades above the collector threshold.

## Backtest

```sh
cargo run --release -p smartmoney-backtest                # default sample
cargo run --release -p smartmoney-backtest -- --markets-per-niche-tag 20 --json results.json
```

The backtest samples recently ended markets per Gamma tag (niche tags plus
sports/crypto as control group, see `[backtest]`), fetches **all** trades of
each market and scores every candidate BUY with only the data available at
its timestamp:

- 24h volume = trades of the market in the 24 hours up to the trade,
- concentration = the wallet's buys in the previous `wallet_history_days`,
- cluster = earlier buyers of the same outcome only,
- wallet age from `join_date` (the number of markets traded is only known
  as of today and is left out).

Outcomes come from `/v2/resolutions` payouts; when a resolved market has no
payouts there, Gamma's final prices are used if they are exactly 0/1 (both
sources must report "resolved"; disagreements are counted and printed).

It compares three variants: **A** the current additive score with the
$2,000 filter, **B** market type as multiplier, **C** B plus trades with a
potential payout of at least `variant_min_payout_usd`. Per score bucket it
prints the hit rate, the average entry price (market-implied probability),
the edge (hit rate minus price), the insider's return, and the return of
following X minutes later at the next real trade (+ slippage, fees).

Subcommands: `weather` (forecast model vs. market on daily temperature
markets; options `--max-price-age-minutes`, `--halves`), `status-quo`
(buy NO in deadline markets, fees included), Subcommands: `calibration` (price vs. outcome per price bucket, category and
time to market end), `bots` (PnL composition and behaviour of bots, active
traders and leaderboards), plus Subcommands: `score` (default, compares score variants), `insider` (targeted
test on personnel, corporate and legal markets over 365 days, trades from
$500) and `smart` (do wallets that beat the market in period 1 keep doing so
in period 2; ranking only uses markets that ended before the split).

API responses are cached in `backtest_cache/` (ignored by Git), so reruns
with other settings are fast. Each run is stored in `backtest_runs`.
