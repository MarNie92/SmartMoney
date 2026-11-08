# SmartMoney – project context for Claude

## Purpose and safety boundary

SmartMoney detects conspicuous Polymarket wallets (possible insiders), alerts
via Telegram and tests signals with paper trading. It must **never** submit
real orders or hold keys for trading. Analysis, signals and simulation only.

The user communicates in German; code, commits and docs are in English.

## Architecture

Rust 2021 Cargo workspace, crates prefixed `smartmoney-`:
`core` (types, config), `collector`, `store` (SQLite/sqlx), `scorer`, `bot`
(teloxide), `web` (axum, SSR / light JS), `sim`, `backtest` (CLI), plus the
binary `apps/smartmoney-daemon` running all services on tokio.
Target: headless Oracle Cloud Always Free VM (Ampere, aarch64 Linux); the
dashboard is reached only via Tailscale (no public port) with a token.

## Conventions

- Conventional Commits in English (`feat:`, `fix:`, `test:`, `docs:`,
  `chore:`, `refactor:`), small logical commits, push after each step.
- Larger features on `feature/<name>`, merged into `main` when done.
- Before every commit: `cargo fmt --all`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace` must be green.
- Secrets only via env vars (`Secret` type, redacted Debug, `#[serde(skip)]`
  so they cannot be set in TOML). Never commit `.env`, `config.local.toml`,
  `*.db`, logs.
- Config structs use `#[serde(default, deny_unknown_fields)]`; keep
  `config.example.toml` in sync (a test parses it).
- Env overrides go through `Config::apply_env(lookup)` so tests need no
  process env mutation.
- Timestamps: unix epoch seconds (`INTEGER`); money in USDC (`REAL`).
- Tests are mandatory for scorer, API parsers and sim settlement.
- Check docs.polymarket.com before implementing API calls; ask the user
  before assuming API fields the docs do not clarify.

## Polymarket API notes (verified against live responses 2026-11-08)

- Data API v1 is retired (2026-11-02). Use `/v2/*`: snake_case fields,
  cursor pagination (`cursor` / `pagination.next_cursor`), `offset` is a 400.
- `/v2/trades`: `size` = shares, `price` = USDC/share, `timestamp` = epoch
  seconds, newest first. `filter_type=CASH&filter_amount=2000` filters by
  USDC value (verified). `outcome_index` 999 = unlabeled.
- `/v2/user-stats`: `trades` = distinct markets traded, `join_date` (s, may
  be null), `all_time_pnl.trade_count` = fills. Fallback for wallet age:
  `/v2/activity?user=..&sort_direction=ASC&start=1&limit=1`.
- `/v2/positions?user=`: `current_value`, `total_cost_usdc` (USDC).
- `/v2/resolutions?condition=`: `status` ("resolved" when final),
  `payouts` in micro-USDC per share (`[0, 1000000]` = outcome 1 won).
- Gamma `/markets?condition_ids=..&include_tag=true&closed=any`:
  `outcomes`, `outcomePrices`, `clobTokenIds` are JSON arrays inside strings;
  `category` is empty, so the category is derived from tag slugs + question
  (`smartmoney_core::classify_market`). Dates: RFC 3339 or
  `2026-11-08 16:36:53.76+00`.
- CLOB `/book`: price/size are strings, **bids ascending and asks descending
  (best last)**, `timestamp` in ms. We normalize to best-first. `/midpoint`
  returns `{"mid": ".."}`. `/prices-history` `t` is epoch seconds.
- Rate limits (per 10 s): Data v2 general 800, `/v2/trades` 300; Gamma
  `/markets` 300; CLOB `/book` 1500; 429 + `Retry-After`. Our client paces
  to `max_requests_per_10s` (default 50) and retries 429/5xx with backoff.
- Volume: ~7 trades >= $2,000 per minute (2026-11-08), mostly sports.

## Environment notes

- Dev machine: Windows 11, Rust via rustup (`~/.cargo/bin`, may need to be
  added to PATH in Git Bash), MSVC Build Tools 2022 installed.

## Current status

Milestones 1-2 done. Collector (`smartmoney-collector`): `PolymarketClient`
(read-only, pacing + backoff), `Collector::poll_once` (dedup via UNIQUE,
high-water mark in `kv_state` with 300 s overlap, first run 1 page only),
enrichment (market/wallet cached `enrich_cache_minutes`), order book snapshot
per trade, resolution check every 10 min. Emits `CollectorEvent` over an mpsc
channel; the daemon currently only logs them.
Tests: parser tests on recorded fixtures (`tests/fixtures`), wiremock
end-to-end test, live tests (`--ignored`).
Next: milestone 3 – scorer consuming `CollectorEvent::TradeStored`.
