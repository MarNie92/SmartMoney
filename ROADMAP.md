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
- [x] Robustness of copying top weather wallets: failed (only one of three
      splits positive, latest split negative, no plateau across group size
      and slippage). Skill persists, but followers cannot capture it.
- Leads for forward paper tests: copy rolling top-20 weather wallets (strongest);
  crypto favourites >= 90¢; large favourite
  trades at 95-98¢ held >= 7 days

## Phase 1d: Weather model v2 (pre-registered criterion, 2026-11-12)

Model: 7 global models (Open-Meteo previous-runs: ECMWF IFS, GFS, ICON,
GEM, JMA, UKMO, Meteo-France), each bias-corrected per station, kind and
lead against the station's routine METAR observations (IEM archive) of the
last 60 resolved days, combined by inverse error variance; spread = RMSE of
the combination x inflation. No observation of the target day or the day
before is used.

Success only if ALL hold (decided before the first run):
1. Brier score better than the market price, 95 % interval of the
   advantage entirely above 0.
2. Only market prices at most 60 minutes old at the decision time.
3. Better than the market (interval above 0) in both halves of the period.
4. Trading threshold chosen on the first half only; taker return after
   fees and slippage positive on the second half.

If any check fails: stop the strategy search (no paper trading).

- [x] Implemented and run (2026-11-12, 9461 events, 51 stations, 50 with
      184 days of METAR observations): **FAILED all accuracy checks.**
      Lead 1 (fresh prices): Brier model 0.1034 vs market 0.0908, advantage
      -0.0126 (-0.0136..-0.0115); worse in both halves; threshold 20 pts
      chosen on half 1 (+19.2 %) gives -7.5 % on half 2. Lead 2 likewise
      (-0.0104). The market is much sharper than the model (market puts
      50-70 % on 2180 buckets and is calibrated; the model rarely exceeds
      50 %). Per the pre-registered rule: strategy search stopped.

## Phase 1e: Learn from expert behaviour (2026-11-12)

- [x] Fingerprint of the top 40 weather wallets: they trade on the event
      day itself, right after the hourly METAR observations (:51-:53),
      mostly 10-15 local time, mid-priced buckets. Their information source
      is public: live station observations (nowcasting).
- [x] Nowcast backtest (`weather-nowcast`), pre-registered 2026-11-12 before
      the first run: decision at 12:58 local on the event day (PRIMARY,
      highest and lowest markets together); 10:58, 14:58 and 16:58 are
      reported for information only and cannot pass the test. Model:
      observed extreme of routine METARs up to the decision (hard bound) +
      mean of 7 models' hourly forecasts for the remaining hours (issued
      >= 24 h earlier), corrected by the station's error of that quantity
      at that hour over the last 60 resolved days. Market price: last trade
      >= $2 within 30 minutes before the decision. Success = all four
      checks of Phase 1d (Brier interval > 0 overall and in both halves,
      fresh prices, threshold chosen on half 1 profitable on half 2 after
      fee 0.05 x p x (1-p) and 50 bps slippage).
      **Result: FAILED.** 12:58 primary: Brier model 0.1231 vs market
      0.0862 (advantage -0.037, interval below 0 in both halves); half-2
      return +1.6 % (interval -9.8..+13.0). Every other hour is also worse
      than the market. Diagnostic (`weather-truth-check`): observed IEM
      extremes fall in the winning bucket for 98.6 % of US (F) events but
      only 92.1 % of C events (off by one bucket at rounding edges).
      Exploratory US-only rerun (chosen after seeing results, not
      confirmatory): still clearly worse than the market at every hour
      (12:58 advantage -0.031). The market already prices live station
      data faster and more precisely than hourly METAR + 24 h old
      forecasts.

## Phase 1f: Real-time weather bot, data feasibility (2026-11-12)

Option A (separate project, never inside SmartMoney): a bot reacting to
live station data. Abort criterion set before the checks: stop if the
data is not fast or precise enough.

- [x] Observations: aviationweather.gov METAR API (free) carries the
      tenths-°C T-group for US stations (resolution precision); international
      stations report whole °C every 20-30 min. Measured public visibility of
      routine US METARs: 2.7-3.3 min after the observation time (10 stations,
      23:5x UTC cycle); international 3.5-7 min.
- [x] Higher frequency: NWS API 5-minute ASOS values exist but only in whole
      °C, with gaps, and became visible ~22 min late. Synoptic one-minute
      ASOS (experimental) documents 2-5 min latency, HF-METAR 5-9 min. No
      public source faster than the METAR API was found.
- [x] Forecasts: HRRR on AWS (public domain) f00 ~51 min after init,
      f18 ~85 min; NBM station text bulletins (`blend_nbstx`) 38-55 min
      after init with daily max/min (TXN) and spread (XND) per US station.
      Not available for international stations.
- [x] Polymarket: CLOB websocket ~2400 price updates / 90 s for 200 weather
      markets, jitter ~6 ms. Weather volume ~$1.2M / 24 h, only ~30 % at US
      stations (~$370k / day over 16 cities).
- [x] Reaction speed (`weather-reaction`, 120 days, 51k buckets ruled out by
      a routine METAR, 27 misclassified = 0.05 %): stale quotes on dead
      buckets are harvested 0-3 min after the observation, i.e. before the
      METAR is public. US, all dead buckets: harvesters' profit (lower bound)
      $28k in 0-3 min vs $7.8k later over 120 days (~$65 / day for all US
      cities, shared with competitors). Misclassified buckets carried most of
      the late "dead" volume: a wrong call costs the full stake.
- **Result: NO-GO.** The public data arrives after the race is decided; the
  remaining slack is too small to pay for the effort and risk. Not tested:
  NBM/HRRR-based forecasts hours ahead (archive on AWS since 2020); low
  expectation given Phase 1d, would need its own pre-registered criterion.

## Phase 1g: Sharp-line anchor backtest, football (pre-registered 2026-11-13)

Hypothesis: Polymarket pre-match football prices are soft; when they
diverge from the sharpest available bookmaker closing line, the
bookmaker side wins more often than the Polymarket price implies.
Motivation: timing profiles of two successful Reddit-sourced wallets
(esports, multi-sport) show their edge sits in the pre-match window,
not in-play.

Design, registered before the first run (`backtest anchor`):

- Sample: resolved Polymarket moneyline events (home win / draw / away
  win as three binary markets) of Bundesliga, EPL, La Liga, Serie A and
  Ligue 1, seasons 2025/26 and 2026/27 (Polymarket coverage starts
  around Feb 2026, inside the fee era).
- Anchor: closing odds from football-data.co.uk, de-vigged
  proportionally; per match the first available of Pinnacle (`PSC*`),
  Betfair Exchange (`BFEC*`), closing market average (`AvgC*`).
- Market price per outcome: last trade >= $10 in the window 60 to 5
  minutes before `gameStartTime`; no fresh price -> outcome excluded.
- Join: league + unordered team pair (normalised tokens + alias list) +
  kickoff date +-1 day; ambiguous joins are dropped and counted.
  Truth check: Polymarket resolution must equal the CSV full-time
  result; mismatches are excluded and counted, > 2 % blocks the test.
- Evaluation: the Phase 1d criterion (`weather_v2::criterion`,
  match-clustered 95 % CIs). PASS requires all four checks: anchor
  Brier beats the market with CI > 0 overall AND in both halves (split
  at the median kickoff date); divergence threshold chosen on half 1
  from {2, 3, 5, 8, 10} points (min 30 bets); half-2 ROI after fee
  0.05 x p x (1-p) and 50 bps slippage must be positive.
- Accepted, documented bias: closing odds are sampled at kickoff, the
  Polymarket price up to 60 min earlier; late team news moves lines, so
  a PASS overstates what is tradable and must be confirmed by a forward
  paper test with real-time quotes before anything else is built.

**Result: FAILED** (first run 2026-11-13; four leagues, because
Polymarket lists no men's Serie A match events, that tag holds only
specials). Pipeline quality: 1173 of 1222 matches joined, 0 ambiguous,
0 truth mismatches, 3458 of 3519 outcomes priced. Finding: the last
pre-kickoff Polymarket price is statistically identical to the
de-vigged closing line; Brier advantage -0.0001 (CI -0.0003..+0.0002),
per league |advantage| <= 0.0004 with all CIs straddling 0. Divergences
of >= 2 points occur in ~1.6 % of outcomes (27 half-1 bets, ROI +41 %
but CI -80..+163 %); no threshold reaches the required 30 half-1 bets.
Conclusion: top-5 football moneylines on Polymarket are glued to the
sharp line in the final hour before kickoff; a borrowed-sharp-line
strategy has no room there. The pre-match edges of the profiled wallets
must come from earlier windows, smaller markets or selection/luck.

**Phase 1g-b, secondary competitions (pre-registered 2026-11-13 before
the first run):** identical design and criterion, new sample: the seven
Polymarket competitions with moneyline coverage that football-data.co.uk
also covers with closing odds — EFL Championship (E1), 2. Bundesliga
(D2), La Liga 2 (SP2), Serie B (I2), Ligue 2 (F2), Primeira Liga (P1),
Scottish Premiership (SC0), seasons 2025/26 + 2026/27 (`anchor
--secondary`). Not testable for lack of a free closing-odds anchor:
cups (Copa del Rey $25M, DFB-Pokal $14M, FA Cup), MLS, Brasileirão and
K League (football-data's extra files carry non-closing odds only), and
Champions/Europa League (no match moneylines under those tags).

**Result: FAILED** (first run 2026-11-14). Join 2057 of 2293 matches
(0 ambiguous, 0 truth mismatches), 4747 of 6171 outcomes priced
(thinner books than the top 5). Checks 1 and 3 failed: the anchor is
not more accurate overall (advantage -0.0002, CI -0.0010..+0.0006) and
half 1 is negative (-0.0010). The texture does differ from the top-5
leagues: >= 2-point divergences are about twice as common (2.8 % of
outcomes), half 2 is marginally anchor-positive (+0.0006, CI
+0.0000..+0.0011) and the registered 2-point strategy earned +14.4 %
out of sample (101 bets in 85 matches, CI -19..+48, check 4 passed).
Per the pre-registration this is a fail; the historical strategy
search stops here. The only clean continuation is a pre-registered
forward paper test against live sharp quotes (e.g. Betfair's delayed
feed) once the Phase 2/5 signal + paper-trading pipeline exists.

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

## Phase 4: Telegram bot (milestone 5; first slice live 2026-11-14)

- [x] Plain Telegram HTTP API (reqwest, no framework; CLAUDE.md updated)
- [x] Alerts: insider trades at or above the score threshold, paper
      fills and paper settlements with P&L
- [x] Chat whitelist (unknown chats are told their chat id), daily
      digest 19:00 Europe/Berlin, `/status` command
- [ ] Inline buttons: "Sim 10 $ / 50 $ / Ignorieren"
- [ ] More commands: `/wallet`, `/threshold`, `/watch`, `/unwatch`,
      `/top`, `/wallets`, `/portfolio`, `/sim_stats`, `/sim_reset`
- [ ] Richer alert context: current price, move since entry, book depth,
      market link

## Phase 5: Paper trading (milestone 6, pulled forward 2026-11-14)

Forward paper test of the two surviving candidates. Rules fixed below
BEFORE the live start; any later rule change restarts the clock for
that strategy. SmartMoney stays read-only: simulated fills only.

- [x] Engine (`smartmoney-sim`): one account per strategy, taker fills
      by walking the live CLOB book for the stake, fee
      `rate(category) x p x (1 - p)` per share, automatic settlement on
      resolution (tested), equity curve.
- [x] Strategy `favorites98` (live since 2026-11-14) (pre-registered): trigger = stored BUY
      trade >= $1000 at 95.0-98.0¢ whose market question is not a
      deadline pattern (status-quo matcher) and whose end date is
      >= 7 days away; paper-buy the same token with $100, at most one
      position per market; hold to resolution. Evaluation after >= 100
      settled positions or on 2027-02-06, whichever comes first:
      PASS = ROI after fees > 0 with >= 100 settled positions; tail
      losses reported explicitly.
- [x] Strategy `anchor2` (code ready; waits for SMARTMONEY_ODDS_API_KEY) (pre-registered): every poll, upcoming
      moneyline events of the seven Phase 1g-b competitions inside the
      window 60-5 min before kickoff; anchor = de-vigged sharp quote
      (The Odds API, Pinnacle preferred, else median book), Polymarket
      price = CLOB midpoint; |anchor - mid| >= 2 points buys the
      anchor side with $100 (taker, walking the book), at most one
      position per outcome market. Evaluation after >= 150 settled
      bets or on 2027-02-06: PASS = ROI after fees > 0 AND anchor
      Brier beats the entry mid with match-clustered 95 % CI > 0.
- [x] Dashboard page /sim (accounts, positions, equity); still open: P&L, hit rate, edge vs entry price, drawdown per
      strategy; score-based strategies (insider/mirror) reuse the same
      engine later.

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
