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

## Polymarket API notes (verified 2026-11-08)

- Data API v1 is retired (2026-11-02). Use `/v2/*`, responses are
  snake_case, cursor pagination (`cursor` / `pagination.next_cursor`),
  `offset` is a 400.
- `/v2/trades`: `size` = shares, `price` = USDC/share, `timestamp` = epoch
  seconds; `filter_type=CASH&filter_amount=2000` filters by USDC value;
  `condition` (not `market`) filters by condition id.
- `/v2/user-stats`: `trades` = distinct markets traded, `join_date` may be
  null. Earliest activity: `/v2/activity?user=..&sort_direction=ASC&start=1`.
- `/v2/positions?user=`: positions with `current_value`, `total_cost_usdc`.
- Resolution: `/v2/resolutions?condition=` → `status`, `payouts`
  (micro-USDC per share). Gamma has no documented winner field.
- Gamma `outcomes`, `outcomePrices`, `clobTokenIds` are typed as strings
  (probably JSON-encoded arrays – verify against live data).
- CLOB `/book`: price/size are strings; `/prices-history` `t` unit is
  undocumented (likely seconds – verify).
- Rate limits (per 10 s): Data v2 general 800, `/v2/trades` 300; Gamma
  `/markets` 300; CLOB `/book` 1500. Clients get 429 + `Retry-After`.

## Environment notes

- Dev machine: Windows 11, Rust via rustup (`~/.cargo/bin`, may need to be
  added to PATH in Git Bash), MSVC Build Tools 2022 installed.

## Current status

Milestone 1 done: workspace, config (TOML + env, validation, tests), SQLite
schema + `Store` (migrations, kv_state), daemon bootstrap, README.
Next: milestone 2 – collector (`/v2/trades` polling, enrichment, rate
limiting/backoff) and persistence.
