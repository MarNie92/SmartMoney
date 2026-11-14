//! The paper-trading service: consumes scorer events for the favorites
//! strategy, polls upcoming football for the anchor strategy, settles
//! positions on resolution and snapshots equity. Simulation only.

use crate::anchor::{anchor_probs, divergence_side, match_event, team_score, tokens, OddsEvent};
use crate::engine::{mark_equity, plan_taker_buy};
use crate::favorites;
use smartmoney_collector::api::{parse_datetime, GammaEventWithMarkets, GammaMarket};
use smartmoney_collector::PolymarketClient;
use smartmoney_core::config::SimulationConfig;
use smartmoney_core::fees::fee_rate;
use smartmoney_scorer::ScorerEvent;
use smartmoney_store::sim::NewSimPosition;
use smartmoney_store::Store;
use std::sync::Arc;
use tokio::sync::watch;

pub const ODDS_BASE: &str = "https://api.the-odds-api.com/v4";
/// Reject fills when the book absorbs less than this share of the stake.
const MIN_FILL_FRACTION: f64 = 0.5;
const EQUITY_EVERY_S: i64 = 3600;

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// An upcoming moneyline match inside the decision window.
struct Upcoming {
    team_a: String,
    team_b: String,
    kickoff: i64,
    /// Markets in [team A wins, draw, team B wins] order.
    markets: [GammaMarket; 3],
}

/// Extract upcoming moneyline matches whose kickoff lies in
/// `[now + cutoff, now + window]`.
fn upcoming_moneylines(
    events: &[GammaEventWithMarkets],
    now: i64,
    window_s: i64,
    cutoff_s: i64,
) -> Vec<Upcoming> {
    let mut out = Vec::new();
    for e in events {
        let Some(title) = e.title.as_deref() else {
            continue;
        };
        let Some((team_a, team_b)) = title
            .split_once(" vs. ")
            .or_else(|| title.split_once(" vs "))
        else {
            continue;
        };
        let mls: Vec<&GammaMarket> = e
            .markets
            .iter()
            .filter(|m| m.sports_market_type.as_deref() == Some("moneyline"))
            .collect();
        if mls.len() != 3 {
            continue;
        }
        let Some(kickoff) = mls
            .iter()
            .find_map(|m| parse_datetime(m.game_start_time.as_deref()?))
        else {
            continue;
        };
        if kickoff - now < cutoff_s || kickoff - now > window_s {
            continue;
        }
        let (ta, tb) = (tokens(team_a), tokens(team_b));
        let mut slots: [Option<GammaMarket>; 3] = [None, None, None];
        for m in &mls {
            let q = m.question.as_deref().unwrap_or_default();
            let slot = if q.to_lowercase().contains("end in a draw") {
                1
            } else {
                let team = q
                    .strip_prefix("Will ")
                    .and_then(|r| r.split(" win on").next())
                    .unwrap_or("");
                let t = tokens(team);
                if team_score(&t, &ta) >= team_score(&t, &tb) {
                    0
                } else {
                    2
                }
            };
            if slots[slot].is_some() {
                slots = [None, None, None];
                break;
            }
            slots[slot] = Some((*m).clone());
        }
        if let [Some(a), Some(d), Some(b)] = slots {
            out.push(Upcoming {
                team_a: team_a.trim().to_owned(),
                team_b: team_b.trim().to_owned(),
                kickoff,
                markets: [a, d, b],
            });
        }
    }
    out
}

/// Yes-outcome index and its CLOB token of a binary market.
fn yes_token(m: &GammaMarket) -> Option<(usize, &str)> {
    let yes = m
        .outcomes
        .iter()
        .position(|o| o.eq_ignore_ascii_case("yes"))?;
    Some((yes, m.clob_token_ids.get(yes)?.as_str()))
}

pub struct SimService {
    store: Store,
    client: PolymarketClient,
    cfg: SimulationConfig,
    favorites_account: Option<i64>,
    anchor_account: Option<i64>,
    http: reqwest::Client,
    odds_base: String,
}

impl SimService {
    pub async fn new(
        store: Store,
        client: PolymarketClient,
        cfg: SimulationConfig,
        now: i64,
    ) -> anyhow::Result<Self> {
        let favorites_account = if cfg.favorites.enabled {
            let json = serde_json::to_string(&cfg.favorites)?;
            Some(
                store
                    .ensure_sim_account("favorites98", &json, cfg.favorites.starting_balance_usd, now)
                    .await?
                    .id,
            )
        } else {
            None
        };
        let anchor_account = if cfg.anchor.enabled && cfg.anchor.odds_api_key.is_some() {
            let json = serde_json::to_string(&cfg.anchor)?;
            Some(
                store
                    .ensure_sim_account("anchor2", &json, cfg.anchor.starting_balance_usd, now)
                    .await?
                    .id,
            )
        } else {
            None
        };
        Ok(Self {
            store,
            client,
            cfg,
            favorites_account,
            anchor_account,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()?,
            odds_base: ODDS_BASE.to_owned(),
        })
    }

    /// Override The Odds API base URL (tests).
    pub fn with_odds_base(mut self, base: &str) -> Self {
        self.odds_base = base.to_owned();
        self
    }

    pub fn favorites_active(&self) -> bool {
        self.favorites_account.is_some()
    }

    pub fn anchor_active(&self) -> bool {
        self.anchor_account.is_some()
    }

    /// React to a scorer event; errors are logged, never fatal.
    pub async fn handle_event(&self, event: &ScorerEvent) {
        let result = match event {
            ScorerEvent::Scored { trade_id, .. } => self.try_favorites(*trade_id).await,
            ScorerEvent::MarketResolved { condition_id } => {
                self.settle_condition(condition_id).await
            }
        };
        if let Err(e) = result {
            tracing::error!(error = %e, "paper trading step failed");
        }
    }

    async fn try_favorites(&self, trade_id: i64) -> anyhow::Result<()> {
        let Some(account) = self.favorites_account else {
            return Ok(());
        };
        let Some(trade) = self.store.trade(trade_id).await? else {
            return Ok(());
        };
        let Some(market) = self.store.market(&trade.condition_id).await? else {
            return Ok(());
        };
        let now = now_ts();
        match favorites::should_follow(&trade, &market, &self.cfg.favorites, now) {
            Err(reason) => {
                tracing::trace!(trade_id, reason, "favorites98 skip");
                Ok(())
            }
            Ok(()) => {
                if self
                    .store
                    .has_sim_position(account, &trade.condition_id)
                    .await?
                {
                    return Ok(());
                }
                self.open_position(
                    account,
                    "favorites98",
                    &trade.condition_id,
                    &trade.token_id,
                    trade.outcome_index,
                    Some(trade.price),
                    fee_rate(market.market.category),
                    self.cfg.favorites.stake_usd,
                    market.market.question.as_deref(),
                )
                .await
            }
        }
    }

    /// Simulated taker buy against the live book. Returns whether a
    /// position was opened.
    #[allow(clippy::too_many_arguments)]
    async fn open_position(
        &self,
        account_id: i64,
        strategy: &str,
        condition_id: &str,
        token_id: &str,
        outcome_index: Option<i64>,
        trigger_price: Option<f64>,
        fee: f64,
        stake: f64,
        label: Option<&str>,
    ) -> anyhow::Result<()> {
        let decided_at = now_ts();
        let book = self.client.book(token_id).await?;
        let snapshot = book.to_snapshot(decided_at);
        let Some(plan) = plan_taker_buy(&snapshot.asks, stake, fee) else {
            tracing::warn!(strategy, condition_id, "no asks: skipping paper fill");
            return Ok(());
        };
        if plan.filled_fraction(stake) < MIN_FILL_FRACTION {
            tracing::warn!(
                strategy,
                condition_id,
                filled = format!("{:.0}%", plan.filled_fraction(stake) * 100.0),
                "book too thin: skipping paper fill"
            );
            return Ok(());
        }
        let position = NewSimPosition {
            account_id,
            score_id: None,
            condition_id: condition_id.to_owned(),
            token_id: token_id.to_owned(),
            outcome_index,
            origin: "auto".into(),
            shares: plan.shares,
            avg_price: plan.avg_price,
            cost_usd: plan.cost_usd,
            fees_usd: plan.fee_usd,
            trigger_price,
            decided_at,
            filled_at: now_ts(),
            snapshot_id: None,
        };
        match self.store.open_sim_position(&position).await {
            Ok(_) => {
                tracing::warn!(
                    "PAPER {strategy}: BUY {:.1} Anteile @ {:.3} (${:.2} inkl. ${:.2} Gebühr) {}",
                    plan.shares,
                    plan.avg_price,
                    plan.cost_usd,
                    plan.fee_usd,
                    label.unwrap_or(condition_id)
                );
                Ok(())
            }
            Err(smartmoney_store::StoreError::Constraint(_)) => {
                tracing::warn!(strategy, "paper cash exhausted: signal skipped");
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Settle all open paper positions of a resolved market.
    pub async fn settle_condition(&self, condition_id: &str) -> anyhow::Result<()> {
        let positions = self
            .store
            .open_sim_positions_by_condition(condition_id)
            .await?;
        if positions.is_empty() {
            return Ok(());
        }
        let Some(market) = self.store.market(condition_id).await? else {
            return Ok(());
        };
        let Some(payouts) = market.payouts else {
            return Ok(());
        };
        let now = now_ts();
        for p in positions {
            let Some(per_share) = p
                .outcome_index
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| payouts.get(i).copied())
            else {
                tracing::error!(position = p.id, "cannot settle: unknown outcome index");
                continue;
            };
            let payout = p.shares * per_share;
            let realized = payout - p.cost_usd;
            self.store
                .settle_sim_position(p.id, payout, realized, now)
                .await?;
            tracing::warn!(
                "PAPER Abrechnung: {:+.2}$ ({} @ {per_share}) {}",
                realized,
                p.shares.round(),
                p.title.as_deref().unwrap_or(condition_id)
            );
        }
        Ok(())
    }

    /// Equity = cash + marked open positions, using the latest stored
    /// market prices (cost price when none is known).
    pub async fn snapshot_equity(&self, now: i64) -> anyhow::Result<()> {
        let accounts = self.store.sim_accounts().await?;
        let open = self.store.open_sim_positions().await?;
        for account in accounts {
            let mut marks = Vec::new();
            for p in open.iter().filter(|p| p.account_id == account.id) {
                let mark = match self.store.market(&p.condition_id).await? {
                    Some(m) => p
                        .outcome_index
                        .and_then(|i| usize::try_from(i).ok())
                        .and_then(|i| m.market.outcome_prices.get(i).copied())
                        .filter(|v| (0.0..=1.0).contains(v))
                        .unwrap_or(p.avg_price),
                    None => p.avg_price,
                };
                marks.push((p.shares, mark));
            }
            let equity = mark_equity(account.cash, &marks);
            self.store
                .insert_sim_equity(account.id, now, equity)
                .await?;
        }
        Ok(())
    }

    /// Credits already spent at The Odds API this calendar month.
    async fn odds_credits(&self) -> anyhow::Result<(String, u32)> {
        let key = format!("sim.odds_credits.{}", chrono::Utc::now().format("%Y-%m"));
        let used = self
            .store
            .get_state(&key)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Ok((key, used))
    }

    async fn fetch_odds(&self, sport: &str) -> anyhow::Result<Vec<OddsEvent>> {
        let key = self
            .cfg
            .anchor
            .odds_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no odds api key"))?;
        let url = format!("{}/sports/{sport}/odds", self.odds_base);
        let response = self
            .http
            .get(&url)
            .query(&[
                ("apiKey", key.expose()),
                ("regions", "eu"),
                ("markets", "h2h"),
                ("oddsFormat", "decimal"),
            ])
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // Never log the URL here: it carries the key.
            anyhow::bail!("odds api {sport}: HTTP {status}");
        }
        Ok(response.json().await?)
    }

    /// One anchor poll: look at upcoming matches inside the decision
    /// window and paper-trade divergences from the de-vigged sharp quote.
    pub async fn anchor_tick(&self, now: i64) -> anyhow::Result<()> {
        let Some(account) = self.anchor_account else {
            return Ok(());
        };
        let a = &self.cfg.anchor;
        let (window_s, cutoff_s) = (a.window_minutes * 60, a.cutoff_minutes * 60);
        for (tag, sport) in &a.sports {
            let events = match self.client.open_events(tag, 50).await {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(tag, error = %e, "gamma open events failed");
                    continue;
                }
            };
            let upcoming = upcoming_moneylines(&events, now, window_s, cutoff_s);
            // Only matches that still need a decision justify an odds call.
            let mut pending = Vec::new();
            for u in upcoming {
                let mut open_slots = Vec::new();
                for (i, m) in u.markets.iter().enumerate() {
                    if !self.store.has_sim_position(account, &m.condition_id).await? {
                        open_slots.push(i);
                    }
                }
                if !open_slots.is_empty() {
                    pending.push((u, open_slots));
                }
            }
            if pending.is_empty() {
                continue;
            }
            let (credit_key, used) = self.odds_credits().await?;
            if used >= a.monthly_credit_cap {
                tracing::warn!(used, cap = a.monthly_credit_cap, "odds api budget exhausted");
                return Ok(());
            }
            let odds = match self.fetch_odds(sport).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(sport, error = %e, "odds fetch failed");
                    continue;
                }
            };
            self.store
                .set_state(&credit_key, &(used + 1).to_string(), now)
                .await?;
            for (u, open_slots) in pending {
                let Some((ev, a_home)) =
                    match_event(&u.team_a, &u.team_b, u.kickoff, &odds)
                else {
                    tracing::info!(
                        "anchor2: kein Quoten-Gegenstück für {} vs. {}",
                        u.team_a,
                        u.team_b
                    );
                    continue;
                };
                let Some(anchor) = anchor_probs(ev, a_home) else {
                    continue;
                };
                for i in open_slots {
                    let market = &u.markets[i];
                    let Some((yes_idx, token)) = yes_token(market) else {
                        continue;
                    };
                    let Ok(Some(mid)) = self.client.midpoint(token).await else {
                        continue;
                    };
                    let Some(buy_yes) = divergence_side(anchor[i], mid, a.threshold) else {
                        continue;
                    };
                    let (buy_idx, trigger) = if buy_yes {
                        (yes_idx, mid)
                    } else {
                        (1 - yes_idx, 1.0 - mid)
                    };
                    let Some(buy_token) = market.clob_token_ids.get(buy_idx) else {
                        continue;
                    };
                    let record = market.clone().into_record(None, now);
                    let fee = fee_rate(record.category);
                    self.store.upsert_market(&record).await?;
                    let label = format!(
                        "{} ({}; Anker {:.3} vs. Mitte {:.3})",
                        market.question.as_deref().unwrap_or("?"),
                        if buy_yes { "YES" } else { "NO" },
                        anchor[i],
                        mid
                    );
                    self.open_position(
                        account,
                        "anchor2",
                        &market.condition_id,
                        buy_token,
                        Some(buy_idx as i64),
                        Some(trigger),
                        fee,
                        a.stake_usd,
                        Some(&label),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Maintenance loop: anchor polling and hourly equity snapshots.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let poll = std::time::Duration::from_secs(self.cfg.anchor.poll_seconds.max(60));
        let mut last_equity = 0i64;
        loop {
            let now = now_ts();
            if self.anchor_active() {
                if let Err(e) = self.anchor_tick(now).await {
                    tracing::error!(error = %e, "anchor tick failed");
                }
            }
            if now - last_equity >= EQUITY_EVERY_S {
                if let Err(e) = self.snapshot_equity(now).await {
                    tracing::error!(error = %e, "equity snapshot failed");
                } else {
                    last_equity = now;
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(poll) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gamma_market(q: &str, cond: &str) -> GammaMarket {
        serde_json::from_value(serde_json::json!({
            "id": "1",
            "conditionId": cond,
            "question": q,
            "sportsMarketType": "moneyline",
            "gameStartTime": "2026-11-14 15:00:00+00",
            "outcomes": "[\"Yes\", \"No\"]",
            "clobTokenIds": "[\"tok-yes\", \"tok-no\"]",
            "closed": false
        }))
        .unwrap()
    }

    #[test]
    fn extracts_upcoming_matches_in_window() {
        let kickoff = parse_datetime("2026-11-14 15:00:00+00").unwrap();
        let event: GammaEventWithMarkets = GammaEventWithMarkets::from_value(&serde_json::json!({
            "id": "e1",
            "title": "Hull City vs. Sheffield Wednesday",
            "markets": []
        }))
        .unwrap();
        let mut event = event;
        event.markets = vec![
            gamma_market("Will Hull City win on 2026-11-14?", "0xa"),
            gamma_market("Will Hull City vs. Sheffield Wednesday end in a draw?", "0xd"),
            gamma_market("Will Sheffield Wednesday win on 2026-11-14?", "0xb"),
        ];
        let events = vec![event];
        // 30 minutes before kickoff: inside the 60-5 window.
        let got = upcoming_moneylines(&events, kickoff - 1800, 3600, 300);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].team_a, "Hull City");
        assert_eq!(got[0].markets[0].condition_id, "0xa");
        assert_eq!(got[0].markets[1].condition_id, "0xd");
        assert_eq!(got[0].markets[2].condition_id, "0xb");
        // Too early and too late.
        assert!(upcoming_moneylines(&events, kickoff - 7200, 3600, 300).is_empty());
        assert!(upcoming_moneylines(&events, kickoff - 120, 3600, 300).is_empty());
        // Yes token resolution.
        assert_eq!(yes_token(&events[0].markets[0]).unwrap(), (0, "tok-yes"));
    }
}
