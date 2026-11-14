//! Strategy "anchor2" (ROADMAP Phase 5, pre-registered 2026-11-14):
//! pre-kickoff divergence of secondary-league moneylines from the
//! de-vigged sharp quote (The Odds API). Pure decision logic; polling
//! and fills live in `service`.

use serde::Deserialize;

/// One event from The Odds API (`/v4/sports/{sport}/odds`).
#[derive(Debug, Clone, Deserialize)]
pub struct OddsEvent {
    pub commence_time: String,
    pub home_team: String,
    pub away_team: String,
    #[serde(default)]
    pub bookmakers: Vec<Bookmaker>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bookmaker {
    pub key: String,
    #[serde(default)]
    pub markets: Vec<OddsMarket>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OddsMarket {
    pub key: String,
    #[serde(default)]
    pub outcomes: Vec<OddsOutcome>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OddsOutcome {
    pub name: String,
    pub price: f64,
}

/// Lowercased, diacritic-folded tokens of a club name.
pub fn tokens(name: &str) -> Vec<String> {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        for l in c.to_lowercase() {
            match l {
                'ä' | 'à' | 'á' | 'â' => out.push('a'),
                'ö' | 'ò' | 'ó' | 'ô' | 'ø' => out.push('o'),
                'ü' | 'ù' | 'ú' | 'û' => out.push('u'),
                'é' | 'è' | 'ê' | 'ë' => out.push('e'),
                'í' | 'ì' | 'î' => out.push('i'),
                'ñ' => out.push('n'),
                'ç' => out.push('c'),
                'ß' => out.push_str("ss"),
                c if c.is_alphanumeric() => out.push(c),
                _ => out.push(' '),
            }
        }
    }
    out.split_whitespace().map(str::to_owned).collect()
}

/// Shared tokens of at least four characters (both APIs use full club
/// names, so this is deliberately simple).
pub fn team_score(a: &[String], b: &[String]) -> usize {
    a.iter().filter(|t| t.len() >= 4 && b.contains(t)).count()
        + usize::from(a.join(" ") == b.join(" ")) * 10
}

/// Find the odds event for a Polymarket team pair kicking off at
/// `kickoff` (unix). Returns the event and whether team A is the home
/// side; ambiguity yields `None`.
pub fn match_event<'a>(
    team_a: &str,
    team_b: &str,
    kickoff: i64,
    events: &'a [OddsEvent],
) -> Option<(&'a OddsEvent, bool)> {
    let (a, b) = (tokens(team_a), tokens(team_b));
    let mut best: Option<(usize, &OddsEvent, bool)> = None;
    let mut tie = false;
    for ev in events {
        let Some(start) = chrono::DateTime::parse_from_rfc3339(&ev.commence_time)
            .ok()
            .map(|t| t.timestamp())
        else {
            continue;
        };
        if (start - kickoff).abs() > 45 * 60 {
            continue;
        }
        let (home, away) = (tokens(&ev.home_team), tokens(&ev.away_team));
        for (a_home, (x, y)) in [(true, (&a, &b)), (false, (&b, &a))] {
            let (sh, sa) = (team_score(x, &home), team_score(y, &away));
            if sh == 0 || sa == 0 {
                continue;
            }
            let score = sh + sa;
            match &best {
                Some((bs, bev, bh)) if score == *bs => {
                    if !std::ptr::eq(*bev, ev) || *bh != a_home {
                        tie = true;
                    }
                }
                Some((bs, _, _)) if score < *bs => {}
                _ => {
                    best = Some((score, ev, a_home));
                    tie = false;
                }
            }
        }
    }
    match best {
        Some((_, ev, a_home)) if !tie => Some((ev, a_home)),
        _ => None,
    }
}

/// De-vigged [team A wins, draw, team B wins] from one bookmaker's h2h
/// market.
fn bookmaker_probs(bm: &Bookmaker, ev: &OddsEvent, a_is_home: bool) -> Option<[f64; 3]> {
    let m = bm.markets.iter().find(|m| m.key == "h2h")?;
    if m.outcomes.len() != 3 {
        return None;
    }
    let (mut home, mut away, mut draw) = (None, None, None);
    for o in &m.outcomes {
        if o.price <= 1.0 {
            return None;
        }
        let inv = 1.0 / o.price;
        if o.name == ev.home_team {
            home = Some(inv);
        } else if o.name == ev.away_team {
            away = Some(inv);
        } else {
            draw = Some(inv);
        }
    }
    let (h, d, a) = (home?, draw?, away?);
    let sum = h + d + a;
    let (h, d, a) = (h / sum, d / sum, a / sum);
    Some(if a_is_home { [h, d, a] } else { [a, d, h] })
}

/// Anchor probabilities: Pinnacle when quoted, else the per-component
/// median over all bookmakers, renormalized.
pub fn anchor_probs(ev: &OddsEvent, a_is_home: bool) -> Option<[f64; 3]> {
    if let Some(p) = ev
        .bookmakers
        .iter()
        .find(|b| b.key == "pinnacle")
        .and_then(|b| bookmaker_probs(b, ev, a_is_home))
    {
        return Some(p);
    }
    let all: Vec<[f64; 3]> = ev
        .bookmakers
        .iter()
        .filter_map(|b| bookmaker_probs(b, ev, a_is_home))
        .collect();
    if all.is_empty() {
        return None;
    }
    let median = |i: usize| -> f64 {
        let mut v: Vec<f64> = all.iter().map(|p| p[i]).collect();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let m = [median(0), median(1), median(2)];
    let sum: f64 = m.iter().sum();
    Some([m[0] / sum, m[1] / sum, m[2] / sum])
}

/// Which side to buy for a divergence, if any: `Some(true)` buys YES,
/// `Some(false)` buys NO.
pub fn divergence_side(anchor: f64, midpoint: f64, threshold: f64) -> Option<bool> {
    if anchor - midpoint >= threshold {
        Some(true)
    } else if midpoint - anchor >= threshold {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(name: &str, price: f64) -> OddsOutcome {
        OddsOutcome {
            name: name.into(),
            price,
        }
    }

    fn event(home: &str, away: &str, time: &str, bookmakers: Vec<Bookmaker>) -> OddsEvent {
        OddsEvent {
            commence_time: time.into(),
            home_team: home.into(),
            away_team: away.into(),
            bookmakers,
        }
    }

    fn h2h(key: &str, home: &str, away: &str, prices: [f64; 3]) -> Bookmaker {
        Bookmaker {
            key: key.into(),
            markets: vec![OddsMarket {
                key: "h2h".into(),
                outcomes: vec![
                    outcome(home, prices[0]),
                    outcome("Draw", prices[1]),
                    outcome(away, prices[2]),
                ],
            }],
        }
    }

    #[test]
    fn devig_prefers_pinnacle() {
        let ev = event(
            "Sheffield Wednesday",
            "Queens Park Rangers",
            "2026-11-14T15:00:00Z",
            vec![
                h2h(
                    "bet365",
                    "Sheffield Wednesday",
                    "Queens Park Rangers",
                    [1.5, 4.0, 8.0],
                ),
                h2h(
                    "pinnacle",
                    "Sheffield Wednesday",
                    "Queens Park Rangers",
                    [2.0, 4.0, 4.0],
                ),
            ],
        );
        let p = anchor_probs(&ev, true).unwrap();
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((p[0] - 0.5).abs() < 1e-12, "pinnacle de-vig, home = 50 %");
        // Away perspective: team A is the away side.
        let q = anchor_probs(&ev, false).unwrap();
        assert!((q[0] - p[2]).abs() < 1e-12);
    }

    #[test]
    fn devig_median_without_pinnacle() {
        let mk = |p: [f64; 3]| h2h("x", "H", "A", p);
        let ev = event(
            "H",
            "A",
            "2026-11-14T15:00:00Z",
            vec![
                mk([2.0, 4.0, 4.0]),
                mk([2.2, 4.0, 3.6]),
                mk([2.1, 4.0, 3.8]),
            ],
        );
        let p = anchor_probs(&ev, true).unwrap();
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(p[0] > 0.45 && p[0] < 0.52);
    }

    #[test]
    fn matches_events_with_orientation() {
        let ko = chrono::DateTime::parse_from_rfc3339("2026-11-14T15:00:00Z")
            .unwrap()
            .timestamp();
        let events = vec![
            event(
                "Sheffield Wednesday",
                "Hull City",
                "2026-11-14T15:00:00Z",
                vec![],
            ),
            event(
                "Sheffield United",
                "Queens Park Rangers",
                "2026-11-14T15:00:00Z",
                vec![],
            ),
            event("Hearts", "Celtic", "2026-11-14T19:00:00Z", vec![]),
        ];
        let (ev, a_home) = match_event("Hull City", "Sheffield Wednesday", ko, &events).unwrap();
        assert_eq!(ev.home_team, "Sheffield Wednesday");
        assert!(!a_home);
        // Kickoff four hours away from the listed events: no match.
        assert!(match_event("Hearts", "Celtic", ko, &events).is_none());
        // Identical team sets at the same time would be ambiguous.
        let dup = vec![
            event("Hull City", "Stoke City", "2026-11-14T15:00:00Z", vec![]),
            event("Hull City", "Stoke City", "2026-11-14T15:00:00Z", vec![]),
        ];
        assert!(match_event("Hull City", "Stoke City", ko, &dup).is_none());
    }

    #[test]
    fn divergence_rule() {
        assert_eq!(divergence_side(0.45, 0.42, 0.02), Some(true));
        assert_eq!(divergence_side(0.40, 0.43, 0.02), Some(false));
        assert_eq!(divergence_side(0.42, 0.41, 0.02), None);
    }
}
