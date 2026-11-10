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

The plan lives in `ROADMAP.md` (phases 1-8); update its checkboxes as work
progresses. Direction since 2026-11-10: insider signals and mirror signals
(proven wallets) share one signal pipeline (dashboard, Telegram, paper
trading per strategy).


Milestones 1-3 done.
- Collector (`smartmoney-collector`): `PolymarketClient` (read-only, pacing +
  backoff), `Collector::poll_once` (dedup via UNIQUE, high-water mark in
  `kv_state` with 300 s overlap, first run 1 page only), enrichment
  (market/wallet cached `enrich_cache_minutes`), order book snapshot per
  trade, resolution check every 10 min. Emits `CollectorEvent`.
- Scorer (`smartmoney-scorer`): pure `model::score(ScoreInput, cfg)` (shared
  with the backtest; no I/O, measured at trade time), `Scorer` service
  consumes `CollectorEvent`, persists `scores` (upsert per trade,
  `SCORER_VERSION`), backfills unscored trades on start, emits
  `ScorerEvent::{Scored, MarketResolved}`. Runtime threshold override in
  `kv_state` key `scoring.alert_threshold` (`effective_threshold`).
- Daemon pipeline: collector -> scorer -> logger (logs `ALERT` lines). Bot and
  sim will replace the logger as consumers of `ScorerEvent`.
- Mini dashboard pulled forward from milestone 7 (user wanted visual
  feedback): `smartmoney-web` with axum + maud (server-rendered), feed with
  breakdown strip + live refresh (`/live` fragment), wallet page, token
  middleware (cookie `sm_token` or `?token=`), loopback-only without token.
  German UI copy. Design: cool fog palette + dark mode, Archivo (Google
  Fonts, condensed for scores), amber for alerts. Visual check via headless
  Edge screenshots (Edge enforces ~500 px min width; use an iframe for 390 px).
  Milestone 7 still adds watchlist, simulation, backtest views.
- Live observation 2026-11-08: most large trades are sports; asymmetry
  rarely fires; a fresh wallet going all-in on a thin sports sub-market can
  reach ~60 without any niche-market signal.

- Backtest (`smartmoney-backtest`, milestone 4): samples ended markets per
  Gamma tag via `/events?tag_slug=..&closed=true&end_date_max=..`, loads all
  trades per market (`/v2/trades?condition=`), wallet facts (`user-stats`
  join_date, buys of the last 30 days), evaluates variants A (additive),
  B (`ScoreMode::Multiplicative`), C (B + payout filter with min price 2¢).
  Pure `engine::evaluate` with no-lookahead tests. Responses cached in
  `backtest_cache/` (rerun from cache ~20 s).
  Resolution: `/v2/resolutions` often has no `payouts` (UMA question rows
  only carry an undocumented `price`); fallback = Gamma final prices when
  both report resolved and prices are exactly 0/1 or 0.5 (0 mismatches in
  76 markets having both).
- Backtest result 2026-11-09 (521 markets, 375k trades, 638 trades >= $2k):
  big trades are mostly favorites (avg 89¢), hit rate == price (no edge
  overall); score buckets not monotonic; A 60-80 bucket +8% edge but from
  only 11 markets (politics: 6) -> not robust; B yields no score >= 60;
  zero "fresh wallet + longshot" trades; payout filter without a price
  floor only adds 0.1¢ dust buys. Follow slippage is small (~+1% at 15 min).
  Conclusion: no evidence of a usable signal in a random recent sample;
  insider cases are rare, a targeted niche sample is the next step.
- Backtest subcommands: `score` (default), `insider` (personnel/corporate/
  legal only, 365 d, >= $500), `smart` (wallet edge persistence, period split;
  copy test follows period-2 buys).
- Insider test 2026-11-10 (1131 niche markets, 14k trades >= $500 in 847
  markets): overall edge -0.7 %; score buckets not predictive (80-100 bucket:
  -1.2 % edge, -45 % ROI); "fresh wallet + longshot" exists (60 trades, 16
  markets) but hit rate 5.0 % vs price 5.8 % -> no edge. Insider score
  hypothesis rejected for now.
- Smart test 2026-11-10: only 75 period-1 markets (sample is "most recently
  ended", so few markets end before the split) -> 64 wallets. Spearman +0.16;
  top quintile (12 wallets) P1 +15.6 % -> P2 +7.1 % (95 % +4.0..+10.3) =
  first significant persistence. But copying their buys: -5 %. Their edge
  likely comes from sells (fading overpriced outcomes), which the copy test
  ignores. Next: balanced period-1 sample (select markets ending before the
  split explicitly) and copy sells as buys of the opposite outcome.
- Balanced smart test 2026-11-10 (30-day windows, 1808 markets, splits
  270/180/90 d, 206-303 wallets each): rank correlation +0.04 / +0.10 /
  -0.14, top group P2 edge +1.5 % / +2.2 % / -5.7 %. No persistence; Phase 1
  gate failed, mirror signals experimental. Copy ROI averages are noisy
  (longshot outliers); prefer edge metrics.
