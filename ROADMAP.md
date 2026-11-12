# SmartMoney roadmap

Goal: one tool that turns two kinds of signals into alerts and tests them
with paper money:

- **Insider signals**: conspicuous trades (fresh wallet, cheap entry, niche
  market, cluster), scored 0-100.
- **Mirror signals**: trades of wallets with a proven, persistent edge.

Both flow through the same pipeline: dashboard, Telegram, paper trading
with one account per strategy. Paper trading checks each thesis going
forward; the backtest checks it on history. Strategies without evidence stay
visible but are marked experimental.

Status legend: `[x]` done, `[~]` in progress, `[ ]` open.
Each phase runs on a `feature/` branch, ends with green fmt/clippy/tests,
a merge to `main`, a push and a short check-in with the user.

## Phase 0: Foundation (done)

- [x] Workspace, config (TOML + env), SQLite schema, docs
- [x] Collector: large trades, wallet/market enrichment, order book
      snapshots, resolutions
- [x] Insider scorer with breakdown, live scoring
- [x] Mini dashboard (feed, wallet page, token protection)
- [x] Backtest CLI: `score`, `insider`, `smart`
- [x] Findings: insider score shows no edge (521 broad + 1131 niche
      markets); smart-money persistence shows a first significant hint
      (top wallets +7 % edge in period 2), but copying their buys lost money

## Phase 1: Validate smart money (decision gate)

- [x] Balanced sample: 30-day end-date windows over the whole range
- [x] Copy sells as buys of the opposite outcome (what a follower can do)
- [x] Robustness: three split dates (270/180/90 days ago), 90-day periods
- **Result 2026-11-10 (1808 markets, 1.27M trades, 206-303 wallets per
  split): gate failed.** Rank correlation P1->P2 +0.04 / +0.10 / -0.14; top
  group P2 edge +1.5 % / +2.2 % / -5.7 % (all intervals include 0 or are
  negative); copy returns inconsistent and dominated by longshot outliers.
  The earlier +7.1 % came from a small sample. Mirror signals stay
  experimental.
- [x] Last test: full trade history of the 1500 most active sample wallets
      (570 loaded, 930 skipped as bots with > 10k trades; 2.3M trades, 237k
      markets resolved), >= 50 trades in 10 markets (P1) and 30 (P2).
      **Result: weak but real rank persistence** (Spearman +0.21 / +0.15 /
      +0.12, 268-311 wallets per split), **economically too small**: top
      group P2 edge +2.5 % / +1.6 % / -1.6 %, USDC-weighted +0.4 % / -5.1 % /
      +0.9 %; copying after 5-15 min is inconsistent (-6 % to +25 %, driven by
      longshot outliers). Mirroring is not a viable money strategy.
- [ ] Visual report of the result
- **Gate:** if copying the top group is positive across splits, mirror
  signals become a primary strategy; otherwise they stay experimental.

## Phase 1b: Calibration and wallet profiles (done)

- [x] Calibration of prices vs outcomes (by price, category, time to end)
- [x] Who makes money: bots, active traders, leaderboards (PnL composition
      and behaviour)
- Result: no exploitable mispricing after fees except a weak lead
  (crypto favourites >= 90¢, +1.8 % edge, ~+1.5 % after fees, one of ~36
  cells); bots earn as market makers (spread, rebates, rewards), which a
  follower cannot copy. Taker fees (rate * (1 - p) of stake) must be part
  of every simulated fill.

## Phase 1c: Strategy tests

- [x] Status quo (buy NO in deadline markets): not provably profitable after
      fees; dust prints must be filtered (`--min-trade-usd`)
- [x] Own probability model: daily weather markets (9381 events, 120 days)
      vs Open-Meteo forecasts. Market more accurate than the model; apparent
      edge at large disagreements came from stale prices and is negative in
      the recent half. No edge.
- [ ] Read-only arbitrage scanner (outcome sums, complementary prices, net
      of fees): how often, how large, how long
- [x] Weather experts: top-20 weather wallets keep a significant edge
      (+2.9 %); copying them within 1-15 min shows +2.4..+4.4 % after fees,
      not yet significant. Single star wallets are not copyable (edge gone
      within a minute).
- Leads for forward paper tests: copy rolling top-20 weather wallets (strongest);
  crypto favourites >= 90¢; large favourite
  trades at 95-98¢ held >= 7 days

## Phase 2: Signal pipeline

- [ ] `Signal` type and `signals` table: kind (insider / mirror), trade,
      wallet, market, side, price, strength, reasons
- [ ] Strategies as configuration (signal kind, threshold, sizing)
- [ ] Insider scorer emits signals; daemon routes signals to consumers

## Phase 3: Wallet tracking and mirroring

- [ ] Candidate pool: wallets from the collector plus `/v2/leaderboard`
- [ ] Nightly wallet ranking: rolling edge from resolved trades, minimum
      activity, confidence interval; table `wallet_skill`
- [ ] Watchlist: automatic (top ranked) plus manual (`/watch`), with labels
- [ ] Collector polls trades of watched wallets (any size) and emits mirror
      signals with a delay-aware entry price

## Phase 4: Telegram bot (milestone 5)

- [ ] Alerts for both signal kinds: market, side, size, entry and current
      price, move since entry, order book depth, link, reasons
- [ ] Inline buttons: "Sim 10 $ / 50 $ / Ignorieren"
- [ ] Commands: `/wallet`, `/threshold`, `/watch`, `/unwatch`, `/top`,
      `/wallets`, `/status`, `/portfolio`, `/sim_stats`, `/sim_reset`
- [ ] Chat whitelist; per-strategy alert thresholds; daily digest against
      alert noise

## Phase 5: Paper trading (milestone 6)

- [ ] One account per strategy (e.g. insider 60/70/80, mirror top,
      manual)
- [ ] Fills against the order book snapshot plus configurable delay,
      slippage from book depth, fees
- [ ] Mark-to-market, automatic settlement on resolution (tested)
- [ ] Stats: P&L, hit rate, edge vs price, P&L per score bucket and
      category, max drawdown, average move between signal and entry

## Phase 6: Full dashboard (milestone 7)

- [ ] Signal feed for both kinds with filters
- [ ] Wallet leaderboard and watchlist management
- [ ] Strategy comparison with equity curves
- [ ] Backtest results page
- [ ] Installable on the phone home screen (web app manifest)

## Phase 7: Deployment (milestone 8)

- [ ] aarch64 build (native on the VM or cross-compile)
- [ ] systemd service with restart, daily SQLite backup (keep 7)
- [ ] Tailscale guide, token setup
- [ ] Daily live API check with a Telegram message on failure
- [ ] Data retention for order book snapshots and raw JSON

## Phase 8: Evaluation loop (ongoing)

- [ ] After 4-8 weeks: compare paper results per strategy with the
      backtest; switch off strategies without edge
- [ ] Optional: wallet funding origin via Polygon RPC as an extra insider
      feature
- Tauri desktop wrapper: dropped in favour of the installable web app
