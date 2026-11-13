//! Loading for the sharp-line anchor backtest: football-data.co.uk closing
//! odds (cached), Polymarket moneyline events per league tag, and the last
//! pre-kickoff trade per outcome market.

use crate::anchor::{
    devig, find, norm_name, parse_odds_csv, prepare, team_ref, team_score, truth_ok, yes_price,
    Join, MatchRow, PreparedOdds, ODDS_BASE, SEASONS,
};
use anyhow::Context;
use chrono::NaiveDate;
use futures::{stream, StreamExt};
use serde::Serialize;
use smartmoney_collector::api::{parse_datetime, GammaEventWithMarkets, GammaMarket};
use smartmoney_collector::PolymarketClient;
use std::collections::{HashMap, HashSet};

/// Ignore events that ended before Polymarket's football coverage.
const OLDEST_END: &str = "2026-01-01";
const EVENT_PAGE_CAP: usize = 120;
const TRADE_PAGE_CAP: usize = 80;
const CONCURRENCY: usize = 8;

#[derive(Debug, Default, Clone, Serialize)]
pub struct LoadStats {
    pub odds_rows: usize,
    pub events_seen: usize,
    pub moneyline_events: usize,
    pub unresolved: usize,
    pub joined: usize,
    pub ambiguous: usize,
    pub unmatched: usize,
    pub truth_mismatch: usize,
    pub sources: HashMap<&'static str, usize>,
    pub priced_outcomes: usize,
    pub outcomes_total: usize,
    pub paging_capped: usize,
}

pub struct AnchorData {
    pub matches: Vec<MatchRow>,
    /// Condition id -> YES price of the last fresh pre-kickoff trade.
    pub prices: HashMap<String, f64>,
    pub stats: LoadStats,
}

/// A moneyline event reduced to what the join needs.
struct Candidate {
    event_id: String,
    league: String,
    team_a: String,
    team_b: String,
    kickoff: i64,
    /// [a wins, draw, b wins].
    condition_ids: [String; 3],
    yes_index: [i64; 3],
    payout: [f64; 3],
}

/// Yes-index and resolved payout of a binary Yes/No market.
fn resolved_yes(m: &GammaMarket) -> Option<(i64, f64)> {
    if m.closed != Some(true) || m.outcomes.len() != 2 || m.outcome_prices.len() != 2 {
        return None;
    }
    let yes = m
        .outcomes
        .iter()
        .position(|o| o.eq_ignore_ascii_case("yes"))?;
    let py: f64 = m.outcome_prices.get(yes)?.parse().ok()?;
    let pn: f64 = m.outcome_prices.get(1 - yes)?.parse().ok()?;
    if !((py == 0.0 || py == 1.0) && (pn - (1.0 - py)).abs() < 1e-9) {
        return None;
    }
    Some((yes as i64, py))
}

/// Split an event title into the two team names.
fn split_title(title: &str) -> Option<(String, String)> {
    for sep in [" vs. ", " vs ", " Vs. ", " Vs "] {
        if let Some((a, b)) = title.split_once(sep) {
            return Some((a.trim().to_owned(), b.trim().to_owned()));
        }
    }
    None
}

/// Build a join candidate from a moneyline event, or count why not.
fn candidate(e: &GammaEventWithMarkets, league: &str, stats: &mut LoadStats) -> Option<Candidate> {
    let title = e.title.as_deref()?;
    let (team_a, team_b) = split_title(title)?;
    let mls: Vec<&GammaMarket> = e
        .markets
        .iter()
        .filter(|m| m.sports_market_type.as_deref() == Some("moneyline"))
        .collect();
    if mls.len() != 3 {
        return None;
    }
    stats.moneyline_events += 1;
    let kickoff = mls
        .iter()
        .find_map(|m| parse_datetime(m.game_start_time.as_deref()?))?;
    // Classify each market as team A win / draw / team B win.
    let (a_norm, b_norm) = (norm_name(&team_a), norm_name(&team_b));
    let (a_ref, b_ref) = (team_ref(&team_a), team_ref(&team_b));
    let mut slots: [Option<(String, i64, f64)>; 3] = [None, None, None];
    for m in &mls {
        let q = m.question.as_deref().unwrap_or("");
        let slot = if q.to_lowercase().contains("end in a draw") {
            1
        } else {
            let team = q
                .strip_prefix("Will ")
                .and_then(|r| r.split(" win on").next())
                .unwrap_or("");
            let n = norm_name(team);
            if n == a_norm {
                0
            } else if n == b_norm {
                2
            } else {
                let r = team_ref(team);
                if team_score(&r, &a_ref) >= team_score(&r, &b_ref) {
                    0
                } else {
                    2
                }
            }
        };
        let Some((yes, payout)) = resolved_yes(m) else {
            stats.unresolved += 1;
            return None;
        };
        if slots[slot].is_some() {
            return None;
        }
        slots[slot] = Some((m.condition_id.clone(), yes, payout));
    }
    let [Some(a), Some(d), Some(b)] = slots else {
        return None;
    };
    Some(Candidate {
        event_id: e.id.clone()?,
        league: league.to_owned(),
        team_a,
        team_b,
        kickoff,
        condition_ids: [a.0, d.0, b.0],
        yes_index: [a.1, d.1, b.1],
        payout: [a.2, d.2, b.2],
    })
}

async fn load_odds(
    client: &PolymarketClient,
    leagues: &[(&str, &str)],
) -> anyhow::Result<HashMap<String, Vec<PreparedOdds>>> {
    let today = chrono::Utc::now().date_naive().to_string();
    let mut out: HashMap<String, Vec<PreparedOdds>> = HashMap::new();
    for (slug, div) in leagues.iter().copied() {
        let mut rows = Vec::new();
        for season in SEASONS {
            let url = format!("{ODDS_BASE}/{season}/{div}.csv");
            // The current season's file grows weekly: refresh its cache daily.
            let query: Vec<(&str, String)> = if season == *SEASONS.last().unwrap() {
                vec![("v", today.clone())]
            } else {
                Vec::new()
            };
            let (text, cached) = client
                .get_text(&url, &query)
                .await
                .with_context(|| format!("downloading {url}"))?;
            let parsed = parse_odds_csv(&text, slug);
            tracing::info!(
                league = slug,
                season,
                rows = parsed.len(),
                cached,
                "odds csv"
            );
            rows.extend(parsed);
        }
        out.insert(slug.to_owned(), prepare(rows));
    }
    Ok(out)
}

async fn load_events(
    client: &PolymarketClient,
    league: &str,
    stats: &mut LoadStats,
) -> anyhow::Result<Vec<Candidate>> {
    let end_max = (chrono::Utc::now().date_naive() + chrono::Duration::days(1)).to_string();
    let oldest = NaiveDate::parse_from_str(OLDEST_END, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor: Option<String> = None;
    for page in 0..EVENT_PAGE_CAP {
        let (events, next) = client
            .closed_events_keyset(league, &end_max, cursor.as_deref(), 100)
            .await?;
        if events.is_empty() {
            break;
        }
        let mut oldest_seen = i64::MAX;
        for e in &events {
            stats.events_seen += 1;
            if let Some(ts) = e.end_date.as_deref().and_then(parse_datetime) {
                oldest_seen = oldest_seen.min(ts);
            }
            if e.id.as_deref().is_none_or(|id| !seen.insert(id.to_owned())) {
                continue;
            }
            if let Some(c) = candidate(e, league, stats) {
                out.push(c);
            }
        }
        cursor = next;
        if cursor.is_none() || oldest_seen < oldest {
            break;
        }
        if page + 1 == EVENT_PAGE_CAP {
            tracing::warn!(league, "event page cap reached");
        }
    }
    tracing::info!(league, candidates = out.len(), "moneyline events loaded");
    Ok(out)
}

/// Last labeled trade >= `min_usd` in `[kickoff - window, kickoff - cutoff]`,
/// as a YES price. The boolean reports whether paging gave up at the cap.
async fn fetch_price(
    client: &PolymarketClient,
    cond: String,
    kickoff: i64,
    yes_index: i64,
    min_usd: f64,
    window_s: i64,
    cutoff_s: i64,
) -> anyhow::Result<(String, Option<f64>, bool)> {
    let mut cursor: Option<String> = None;
    for _ in 0..TRADE_PAGE_CAP {
        let page = client
            .market_trades_min_cash_page(&cond, min_usd, cursor.as_deref())
            .await?;
        for t in &page.data {
            if t.timestamp > kickoff - cutoff_s {
                continue;
            }
            if t.timestamp < kickoff - window_s {
                return Ok((cond, None, false));
            }
            if let Some(oi) = t.outcome_index {
                if oi != 999 {
                    return Ok((cond, Some(yes_price(t.price, oi, yes_index)), false));
                }
            }
        }
        cursor = page.pagination.and_then(|p| p.next_cursor);
        if cursor.is_none() {
            return Ok((cond, None, false));
        }
    }
    Ok((cond, None, true))
}

pub async fn load(
    client: &PolymarketClient,
    leagues: &[(&str, &str)],
    min_trade_usd: f64,
    window_s: i64,
    cutoff_s: i64,
) -> anyhow::Result<AnchorData> {
    let odds = load_odds(client, leagues).await?;
    let mut stats = LoadStats {
        odds_rows: odds.values().map(Vec::len).sum(),
        ..LoadStats::default()
    };

    let mut candidates = Vec::new();
    for (slug, _) in leagues.iter().copied() {
        candidates.extend(load_events(client, slug, &mut stats).await?);
    }

    // Join each Polymarket match to its CSV row.
    let mut matches = Vec::new();
    for c in &candidates {
        let rows = odds.get(&c.league).map(Vec::as_slice).unwrap_or(&[]);
        let date = chrono::DateTime::from_timestamp(c.kickoff, 0)
            .map(|t| t.date_naive())
            .unwrap_or_default();
        let (a_ref, b_ref) = (team_ref(&c.team_a), team_ref(&c.team_b));
        match find(&a_ref, &b_ref, date, rows) {
            Join::Unique(p, a_home) => {
                let Some(anchor_p) = devig(p.row.odds) else {
                    continue;
                };
                let (cid, yes, pay) = if a_home {
                    (c.condition_ids.clone(), c.yes_index, c.payout)
                } else {
                    (
                        [
                            c.condition_ids[2].clone(),
                            c.condition_ids[1].clone(),
                            c.condition_ids[0].clone(),
                        ],
                        [c.yes_index[2], c.yes_index[1], c.yes_index[0]],
                        [c.payout[2], c.payout[1], c.payout[0]],
                    )
                };
                let m = MatchRow {
                    event_id: c.event_id.clone(),
                    league: c.league.clone(),
                    kickoff: c.kickoff,
                    date,
                    anchor: anchor_p,
                    source: p.row.source,
                    condition_ids: cid,
                    payout: pay,
                    ftr: p.row.ftr,
                };
                if truth_ok(&m) {
                    *stats.sources.entry(p.row.source).or_default() += 1;
                    stats.joined += 1;
                    matches.push((m, yes));
                } else {
                    stats.truth_mismatch += 1;
                }
            }
            Join::Ambiguous => stats.ambiguous += 1,
            Join::None => stats.unmatched += 1,
        }
    }

    // Fetch the pre-kickoff price for every outcome market.
    let tasks: Vec<(String, i64, i64)> = matches
        .iter()
        .flat_map(|(m, yes)| (0..3).map(move |i| (m.condition_ids[i].clone(), m.kickoff, yes[i])))
        .collect();
    stats.outcomes_total = tasks.len();
    let mut prices = HashMap::new();
    let mut results = stream::iter(tasks.into_iter().map(|(cond, kickoff, yes)| {
        fetch_price(
            client,
            cond,
            kickoff,
            yes,
            min_trade_usd,
            window_s,
            cutoff_s,
        )
    }))
    .buffer_unordered(CONCURRENCY);
    let mut done = 0usize;
    while let Some(r) = results.next().await {
        done += 1;
        if done % 500 == 0 {
            tracing::info!(done, total = stats.outcomes_total, "prices fetched");
        }
        let (cond, price, capped) = r?;
        if capped {
            stats.paging_capped += 1;
        }
        if let Some(p) = price {
            prices.insert(cond, p);
        }
    }
    stats.priced_outcomes = prices.len();

    let matches: Vec<MatchRow> = matches.into_iter().map(|(m, _)| m).collect();
    Ok(AnchorData {
        matches,
        prices,
        stats,
    })
}
