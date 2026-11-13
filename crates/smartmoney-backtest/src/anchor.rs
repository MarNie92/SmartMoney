//! Sharp-line anchor backtest (ROADMAP Phase 1g): Polymarket pre-match
//! football moneyline prices vs de-vigged bookmaker closing odds from
//! football-data.co.uk. Pure logic: CSV parsing, de-vigging, team name
//! matching and observation building; loading lives in `anchor_load`.

use crate::weather::{Kind, Observation};
use chrono::NaiveDate;
use std::collections::{BTreeSet, HashMap};

/// (Polymarket tag slug, football-data.co.uk division code).
pub const LEAGUES: [(&str, &str); 5] = [
    ("bundesliga", "D1"),
    ("epl", "E0"),
    ("la-liga", "SP1"),
    ("serie-a", "I1"),
    ("ligue-1", "F1"),
];

/// football-data season directory codes.
pub const SEASONS: [&str; 2] = ["2526", "2627"];

pub const ODDS_BASE: &str = "https://football-data.co.uk/mmz4281";

/// One match row from a football-data CSV with closing odds.
#[derive(Debug, Clone, PartialEq)]
pub struct OddsRow {
    pub league: String,
    pub date: NaiveDate,
    pub home: String,
    pub away: String,
    /// Full-time result: 'H', 'D' or 'A'.
    pub ftr: char,
    /// Closing decimal odds for home / draw / away.
    pub odds: [f64; 3],
    /// Which bookmaker columns supplied the odds.
    pub source: &'static str,
}

/// Odds column triples in registered priority order.
const TRIPLES: [([&str; 3], &str); 3] = [
    (["PSCH", "PSCD", "PSCA"], "Pinnacle"),
    (["BFECH", "BFECD", "BFECA"], "BetfairEx"),
    (["AvgCH", "AvgCD", "AvgCA"], "AvgClose"),
];

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%d/%m/%Y")
        .or_else(|_| NaiveDate::parse_from_str(s, "%d/%m/%y"))
        .ok()
}

/// Parse one football-data CSV. Rows without a result or without a full
/// closing-odds triple are skipped.
pub fn parse_odds_csv(text: &str, league: &str) -> Vec<OddsRow> {
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let cols: Vec<&str> = header
        .trim_start_matches('\u{feff}')
        .split(',')
        .map(str::trim)
        .collect();
    let idx = |name: &str| cols.iter().position(|c| *c == name);
    let (Some(di), Some(hi), Some(ai), Some(fi)) =
        (idx("Date"), idx("HomeTeam"), idx("AwayTeam"), idx("FTR"))
    else {
        return Vec::new();
    };
    let triples: Vec<([usize; 3], &'static str)> = TRIPLES
        .iter()
        .filter_map(|(names, src)| {
            let a = idx(names[0])?;
            let b = idx(names[1])?;
            let c = idx(names[2])?;
            Some(([a, b, c], *src))
        })
        .collect();
    let mut out = Vec::new();
    for line in lines {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let (Some(date), Some(home), Some(away), Some(ftr)) = (
            f.get(di).and_then(|s| parse_date(s)),
            f.get(hi).filter(|s| !s.is_empty()),
            f.get(ai).filter(|s| !s.is_empty()),
            f.get(fi).and_then(|s| s.chars().next()),
        ) else {
            continue;
        };
        if !matches!(ftr, 'H' | 'D' | 'A') {
            continue;
        }
        let Some((odds, source)) = triples.iter().find_map(|(ix, src)| {
            let o: Vec<f64> = ix
                .iter()
                .filter_map(|i| f.get(*i).and_then(|s| s.parse::<f64>().ok()))
                .filter(|v| *v > 1.0)
                .collect();
            (o.len() == 3).then(|| ([o[0], o[1], o[2]], *src))
        }) else {
            continue;
        };
        out.push(OddsRow {
            league: league.to_owned(),
            date,
            home: (*home).to_owned(),
            away: (*away).to_owned(),
            ftr,
            odds,
            source,
        });
    }
    out
}

/// Remove the bookmaker margin proportionally.
pub fn devig(odds: [f64; 3]) -> Option<[f64; 3]> {
    if odds.iter().any(|o| *o <= 1.0) {
        return None;
    }
    let inv: Vec<f64> = odds.iter().map(|o| 1.0 / o).collect();
    let sum: f64 = inv.iter().sum();
    Some([inv[0] / sum, inv[1] / sum, inv[2] / sum])
}

/// Full-name aliases applied after normalisation (both sides).
const FULL_ALIAS: [(&str, &str); 9] = [
    ("ac milan", "milan"),
    ("athletic club", "ath bilbao"),
    ("athletic bilbao", "ath bilbao"),
    ("atletico de madrid", "atletico madrid"),
    ("ath madrid", "atletico madrid"),
    ("paris saint germain", "paris sg"),
    ("inter milan", "inter"),
    ("fc internazionale milano", "inter"),
    ("internazionale", "inter"),
];

/// Token aliases (short forms, diacritic variants).
const TOKEN_ALIAS: [(&str, &str); 14] = [
    ("man", "manchester"),
    ("nottm", "nottingham"),
    ("wolves", "wolverhampton"),
    ("spurs", "tottenham"),
    ("lyonnais", "lyon"),
    ("rennais", "rennes"),
    ("brestois", "brest"),
    ("hamburger", "hamburg"),
    ("espanyol", "espanol"),
    ("munchen", "munich"),
    ("monchengladbach", "gladbach"),
    ("mgladbach", "gladbach"),
    ("koeln", "koln"),
    ("saint", "st"),
];

/// Club-form tokens that carry no identity.
const STOP: [&str; 40] = [
    "fc",
    "cf",
    "sc",
    "ac",
    "afc",
    "cfc",
    "club",
    "calcio",
    "cd",
    "rcd",
    "ud",
    "us",
    "ss",
    "ssc",
    "sv",
    "vfl",
    "vfb",
    "tsg",
    "bsc",
    "fsv",
    "spvgg",
    "aj",
    "as",
    "og",
    "ogc",
    "rc",
    "ca",
    "hsc",
    "sco",
    "osc",
    "losc",
    "acf",
    "bc",
    "cp",
    "fk",
    "sk",
    "stade",
    "olympique",
    "deportivo",
    "sd",
];

/// Shared tokens that are weak evidence (shared city or honorific).
const WEAK: [&str; 18] = [
    "real",
    "united",
    "city",
    "manchester",
    "madrid",
    "milan",
    "athletic",
    "ath",
    "borussia",
    "st",
    "west",
    "paris",
    "union",
    "le",
    "la",
    "los",
    "las",
    "san",
];

/// Short tokens that still identify a club.
const SHORT_OK: [&str; 1] = ["sg"];

fn fold_char(c: char, out: &mut String) {
    let lower = c.to_lowercase();
    for l in lower {
        match l {
            'ä' | 'à' | 'á' | 'â' | 'ã' | 'å' => out.push('a'),
            'ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'ø' => out.push('o'),
            'ü' | 'ù' | 'ú' | 'û' => out.push('u'),
            'é' | 'è' | 'ê' | 'ë' => out.push('e'),
            'í' | 'ì' | 'î' | 'ï' => out.push('i'),
            'ñ' => out.push('n'),
            'ç' => out.push('c'),
            'ß' => out.push_str("ss"),
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
}

/// Lowercased, diacritic-folded name with punctuation collapsed to spaces.
pub fn norm_name(s: &str) -> String {
    let mut t = String::with_capacity(s.len());
    for c in s.chars() {
        fold_char(c, &mut t);
    }
    let joined = t.split_whitespace().collect::<Vec<_>>().join(" ");
    for (from, to) in FULL_ALIAS {
        if joined == from {
            return to.to_owned();
        }
    }
    joined
}

/// A team name prepared for matching.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamRef {
    pub norm: String,
    pub tokens: BTreeSet<String>,
}

pub fn team_ref(s: &str) -> TeamRef {
    let norm = norm_name(s);
    let tokens = norm
        .split(' ')
        .map(|t| {
            TOKEN_ALIAS
                .iter()
                .find(|(f, _)| *f == t)
                .map_or(t, |(_, to)| *to)
                .to_owned()
        })
        .filter(|t| !STOP.contains(&t.as_str()))
        .filter(|t| t.parse::<u32>().is_err())
        .filter(|t| t.len() >= 2)
        .collect();
    TeamRef { norm, tokens }
}

/// Match evidence: 100 for equal normalised names, else 2 per shared
/// distinctive token and 1 per shared weak token. >= 2 counts as a match.
pub fn team_score(a: &TeamRef, b: &TeamRef) -> u32 {
    if a.norm == b.norm {
        return 100;
    }
    a.tokens
        .intersection(&b.tokens)
        .map(|t| {
            if WEAK.contains(&t.as_str()) {
                1
            } else if t.len() >= 3 || SHORT_OK.contains(&t.as_str()) {
                2
            } else {
                0
            }
        })
        .sum()
}

/// An odds row with prepared team references.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedOdds {
    pub row: OddsRow,
    pub home: TeamRef,
    pub away: TeamRef,
}

pub fn prepare(rows: Vec<OddsRow>) -> Vec<PreparedOdds> {
    rows.into_iter()
        .map(|row| PreparedOdds {
            home: team_ref(&row.home),
            away: team_ref(&row.away),
            row,
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub enum Join<'a> {
    /// Unique best candidate; `true` when the first team is the home side.
    Unique(&'a PreparedOdds, bool),
    Ambiguous,
    None,
}

/// Find the CSV row for a Polymarket team pair near a kickoff date.
pub fn find<'a>(a: &TeamRef, b: &TeamRef, date: NaiveDate, rows: &'a [PreparedOdds]) -> Join<'a> {
    let mut best: Option<(u32, &PreparedOdds, bool)> = None;
    let mut tie = false;
    for p in rows {
        if (p.row.date - date).num_days().abs() > 1 {
            continue;
        }
        for (a_home, (x, y)) in [(true, (a, b)), (false, (b, a))] {
            let (sh, sa) = (team_score(x, &p.home), team_score(y, &p.away));
            if sh < 2 || sa < 2 {
                continue;
            }
            let score = sh + sa;
            match &best {
                Some((bs, bp, bh)) if score == *bs => {
                    // The same row in the same orientation cannot tie itself.
                    if !std::ptr::eq(*bp, p) || *bh != a_home {
                        tie = true;
                    }
                }
                Some((bs, _, _)) if score < *bs => {}
                _ => {
                    best = Some((score, p, a_home));
                    tie = false;
                }
            }
        }
    }
    match best {
        Some((_, p, a_home)) if !tie => Join::Unique(p, a_home),
        Some(_) => Join::Ambiguous,
        None => Join::None,
    }
}

/// One joined match: Polymarket moneyline event + anchor probabilities.
/// Index order is always [home, draw, away].
#[derive(Debug, Clone, PartialEq)]
pub struct MatchRow {
    pub event_id: String,
    pub league: String,
    pub kickoff: i64,
    pub date: NaiveDate,
    /// De-vigged anchor probabilities.
    pub anchor: [f64; 3],
    pub source: &'static str,
    pub condition_ids: [String; 3],
    /// Resolved YES payout per outcome market.
    pub payout: [f64; 3],
    pub ftr: char,
}

/// Does the Polymarket resolution agree with the CSV full-time result?
pub fn truth_ok(m: &MatchRow) -> bool {
    let winners: Vec<usize> = (0..3).filter(|i| m.payout[*i] >= 0.999).collect();
    let ftr_idx = match m.ftr {
        'H' => 0,
        'D' => 1,
        _ => 2,
    };
    winners == [ftr_idx]
}

/// YES price of a trade given the traded outcome index.
pub fn yes_price(price: f64, outcome_index: i64, yes_index: i64) -> f64 {
    if outcome_index == yes_index {
        price
    } else {
        1.0 - price
    }
}

pub const OUTCOME_LABELS: [&str; 3] = ["home", "draw", "away"];

/// One observation per outcome market with a fresh pre-kickoff price.
pub fn observations(matches: &[MatchRow], prices: &HashMap<String, f64>) -> Vec<Observation> {
    let mut out = Vec::new();
    for m in matches {
        for (i, label) in OUTCOME_LABELS.iter().enumerate() {
            let Some(p) = prices.get(&m.condition_ids[i]) else {
                continue;
            };
            out.push(Observation {
                event_id: m.event_id.clone(),
                kind: Kind::High,
                station: m.league.clone(),
                date: m.date,
                label: (*label).to_owned(),
                model: m.anchor[i],
                market: *p,
                outcome: m.payout[i],
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV_PSC: &str =
        "\u{feff}Div,Date,Time,HomeTeam,AwayTeam,FTR,PSCH,PSCD,PSCA,AvgCH,AvgCD,AvgCA\n\
        D1,08/11/2026,18:30,Hoffenheim,Schalke 04,H,1.85,3.9,4.1,1.8,3.8,4.0\n\
        D1,09/11/2026,15:30,Bayern Munich,Dortmund,D,,,,1.5,4.5,6.0\n\
        D1,10/11/26,15:30,Freiburg,Union Berlin,X,2.0,3.3,3.9,2.0,3.3,3.9\n";

    #[test]
    fn parses_csv_with_priority_and_fallback() {
        let rows = parse_odds_csv(CSV_PSC, "bundesliga");
        assert_eq!(rows.len(), 2, "row with FTR=X is dropped");
        assert_eq!(rows[0].source, "Pinnacle");
        assert_eq!(rows[0].odds, [1.85, 3.9, 4.1]);
        assert_eq!(rows[0].ftr, 'H');
        assert_eq!(rows[0].date, NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
        // Missing Pinnacle columns fall back to the closing average.
        assert_eq!(rows[1].source, "AvgClose");
        assert_eq!(rows[1].home, "Bayern Munich");
    }

    #[test]
    fn devig_normalises_probabilities() {
        let p = devig([2.0, 4.0, 4.0]).unwrap();
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((p[0] - 0.5).abs() < 1e-12);
        assert_eq!(devig([1.0, 4.0, 4.0]), None);
    }

    fn j(a: &str, b: &str, rows: &[PreparedOdds]) -> Join<'static> {
        // Lifetimes in tests: leak the prepared rows for simplicity.
        let rows: &'static [PreparedOdds] = Box::leak(rows.to_vec().into_boxed_slice());
        find(
            &team_ref(a),
            &team_ref(b),
            NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            rows,
        )
    }

    fn odds_row(home: &str, away: &str, day: u32) -> OddsRow {
        OddsRow {
            league: "x".into(),
            date: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
            home: home.into(),
            away: away.into(),
            ftr: 'H',
            odds: [2.0, 3.5, 3.8],
            source: "Pinnacle",
        }
    }

    #[test]
    fn matches_hard_team_names() {
        let rows = prepare(vec![
            odds_row("Man United", "Nott'm Forest", 8),
            odds_row("Man City", "Wolves", 8),
            odds_row("M'gladbach", "Ein Frankfurt", 8),
            odds_row("Ath Madrid", "Sociedad", 8),
            odds_row("Real Madrid", "Betis", 8),
            odds_row("Paris SG", "St Etienne", 8),
            odds_row("Inter", "Milan", 8),
        ]);
        let cases = [
            ("Manchester United", "Nottingham Forest", 0, true),
            ("Wolverhampton Wanderers", "Manchester City", 1, false),
            ("Eintracht Frankfurt", "Borussia Monchengladbach", 2, false),
            ("Atlético Madrid", "Real Sociedad", 3, true),
            ("Real Betis", "Real Madrid", 4, false),
            ("Paris Saint-Germain", "AS Saint-Étienne", 5, true),
            ("Inter Milan", "AC Milan", 6, true),
        ];
        for (a, b, want_idx, want_home) in cases {
            match j(a, b, &rows) {
                Join::Unique(p, a_home) => {
                    assert_eq!(p.row.home, rows[want_idx].row.home, "{a} vs {b}");
                    assert_eq!(a_home, want_home, "{a} vs {b}");
                }
                other => panic!("{a} vs {b}: {other:?}"),
            }
        }
    }

    #[test]
    fn ambiguous_and_distant_rows() {
        let rows = prepare(vec![
            odds_row("Paris SG", "Lens", 8),
            odds_row("Paris FC", "Lens", 8),
        ]);
        // "Paris FC" normalises to "paris fc" -> exact-name evidence wins.
        match j("Paris FC", "RC Lens", &rows) {
            Join::Unique(p, true) => assert_eq!(p.row.home, "Paris FC"),
            other => panic!("{other:?}"),
        }
        // A match three days away is not considered.
        let far = prepare(vec![odds_row("Freiburg", "Mainz", 12)]);
        assert_eq!(j("SC Freiburg", "FSV Mainz 05", &far), Join::None);
        // Two equally plausible rows -> ambiguous.
        let dup = prepare(vec![
            odds_row("Freiburg", "Mainz", 8),
            odds_row("Freiburg", "Mainz", 9),
        ]);
        assert_eq!(j("SC Freiburg", "FSV Mainz 05", &dup), Join::Ambiguous);
    }

    fn match_row(payout: [f64; 3], ftr: char) -> MatchRow {
        MatchRow {
            event_id: "e1".into(),
            league: "epl".into(),
            kickoff: 1_700_000_000,
            date: NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            anchor: [0.5, 0.3, 0.2],
            source: "Pinnacle",
            condition_ids: ["h".into(), "d".into(), "a".into()],
            payout,
            ftr,
        }
    }

    #[test]
    fn truth_check_and_observations() {
        assert!(truth_ok(&match_row([1.0, 0.0, 0.0], 'H')));
        assert!(!truth_ok(&match_row([1.0, 0.0, 0.0], 'A')));
        assert!(!truth_ok(&match_row([1.0, 1.0, 0.0], 'H')));

        assert!((yes_price(0.3, 0, 0) - 0.3).abs() < 1e-12);
        assert!((yes_price(0.3, 1, 0) - 0.7).abs() < 1e-12);

        let m = match_row([0.0, 1.0, 0.0], 'D');
        let prices = HashMap::from([("h".to_owned(), 0.45), ("a".to_owned(), 0.25)]);
        let obs = observations(&[m], &prices);
        assert_eq!(obs.len(), 2, "draw market has no fresh price");
        let home = obs.iter().find(|o| o.label == "home").unwrap();
        assert!((home.model - 0.5).abs() < 1e-12);
        assert!((home.market - 0.45).abs() < 1e-12);
        assert_eq!(home.outcome, 0.0);
        assert_eq!(home.event_id, "e1");
    }
}
