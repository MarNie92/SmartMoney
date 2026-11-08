//! Market classification from Gamma tag slugs and the market question.
//!
//! Gamma's `category` field is empty for current markets, so the category is
//! derived from tags (`include_tag=true`) plus keywords in the question.
//! Low-insider-value categories (crypto prices, sports) are checked first so
//! that, e.g., a sports market mentioning a "trade" is not marked corporate.

use crate::types::MarketCategory;

const CRYPTO_PRICE_TAGS: &[&str] = &["crypto-prices", "hit-price", "up-or-down", "crypto-price"];
const CRYPTO_TAGS: &[&str] = &["crypto", "bitcoin", "ethereum", "solana", "xrp", "dogecoin"];
const SPORTS_TAGS: &[&str] = &[
    "sports",
    "nfl",
    "nba",
    "mlb",
    "nhl",
    "soccer",
    "football",
    "basketball",
    "baseball",
    "hockey",
    "tennis",
    "golf",
    "ufc",
    "mma",
    "boxing",
    "f1",
    "formula-1",
    "cricket",
    "esports",
    "epl",
    "premier-league",
    "champions-league",
    "la-liga",
    "serie-a",
    "bundesliga",
    "ncaa",
    "cfb",
    "cbb",
    "wnba",
    "olympics",
    "games",
];
const LEGAL_TAGS: &[&str] = &[
    "courts",
    "court",
    "legal",
    "scotus",
    "supreme-court",
    "trials",
    "trial",
    "lawsuit",
    "crime",
    "justice",
];
const CORPORATE_TAGS: &[&str] = &[
    "business",
    "tech",
    "companies",
    "company",
    "earnings",
    "ipo",
    "ipos",
    "mergers",
    "big-tech",
    "ai",
    "stocks",
    "economy",
];
const POLITICS_TAGS: &[&str] = &[
    "politics",
    "elections",
    "geopolitics",
    "world",
    "us-politics",
    "global-elections",
    "trump",
    "congress",
];

const LEGAL_WORDS: &[&str] = &[
    "court",
    "sentenced",
    "convicted",
    "indicted",
    "indictment",
    "lawsuit",
    "verdict",
    "ruling",
    "trial",
    "guilty",
    "acquitted",
    "pardon",
    "extradited",
    "arrested",
    "charged",
];
const PERSONNEL_WORDS: &[&str] = &[
    "resign",
    "fired",
    "out as",
    "next ceo",
    "new ceo",
    "ceo of",
    "step down",
    "steps down",
    "appointed",
    "nominate",
    "nominee",
    "confirmed as",
    "leave office",
    "replaced as",
    "head coach",
    "next pope",
    "chair",
    "named as",
    "hired",
    "departure",
    "leaves",
];
const CORPORATE_WORDS: &[&str] = &[
    "ipo",
    "acquire",
    "acquisition",
    "merger",
    "earnings",
    "announce",
    "release",
    "launch",
    "partnership",
    "delist",
    "bankrupt",
    "layoff",
    "revenue",
    "market cap",
];
const CRYPTO_PRICE_WORDS: &[&str] = &[
    "price of",
    "above $",
    "below $",
    "reach $",
    "dip to $",
    "hit $",
    "up or down",
];

/// Classify a market. `tags` are Gamma tag slugs (market and event tags).
pub fn classify_market(tags: &[String], question: &str) -> MarketCategory {
    let q = question.to_lowercase();
    let has_tag = |list: &[&str]| {
        tags.iter()
            .any(|t| list.contains(&t.to_lowercase().as_str()))
    };
    let has_word = |list: &[&str]| list.iter().any(|w| q.contains(w));

    if has_tag(CRYPTO_PRICE_TAGS) || (has_tag(CRYPTO_TAGS) && has_word(CRYPTO_PRICE_WORDS)) {
        return MarketCategory::CryptoPrice;
    }
    if has_tag(SPORTS_TAGS) {
        return MarketCategory::Sports;
    }
    if has_tag(LEGAL_TAGS) || has_word(LEGAL_WORDS) {
        return MarketCategory::Legal;
    }
    if has_word(PERSONNEL_WORDS) {
        return MarketCategory::Personnel;
    }
    if has_tag(CORPORATE_TAGS) || has_word(CORPORATE_WORDS) {
        return MarketCategory::Corporate;
    }
    if has_tag(POLITICS_TAGS) {
        return MarketCategory::Politics;
    }
    if has_tag(CRYPTO_TAGS) {
        // Crypto but not a price market (e.g. token launches, listings).
        return MarketCategory::Corporate;
    }
    MarketCategory::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn crypto_price_markets() {
        assert_eq!(
            classify_market(
                &tags(&["crypto", "crypto-prices", "bitcoin"]),
                "Will Bitcoin reach $78,000 November 2-8?"
            ),
            MarketCategory::CryptoPrice
        );
        assert_eq!(
            classify_market(&tags(&["crypto"]), "Will ETH be above $5,000 on Friday?"),
            MarketCategory::CryptoPrice
        );
    }

    #[test]
    fn sports_markets() {
        assert_eq!(
            classify_market(&tags(&["sports", "nfl"]), "Spread: Bengals (-4.5)"),
            MarketCategory::Sports
        );
        // Sports wins even if the question contains a personnel keyword.
        assert_eq!(
            classify_market(&tags(&["soccer"]), "Will the head coach be fired?"),
            MarketCategory::Sports
        );
    }

    #[test]
    fn niche_markets() {
        assert_eq!(
            classify_market(&tags(&["politics"]), "Will the Fed chair resign in 2026?"),
            MarketCategory::Personnel
        );
        assert_eq!(
            classify_market(&tags(&["business"]), "Who will be the next CEO of Apple?"),
            MarketCategory::Personnel
        );
        assert_eq!(
            classify_market(&tags(&[]), "Will the Supreme Court rule in favor of X?"),
            MarketCategory::Legal
        );
        assert_eq!(
            classify_market(&tags(&["tech"]), "Will OpenAI announce GPT-6 by June?"),
            MarketCategory::Corporate
        );
    }

    #[test]
    fn politics_and_other() {
        assert_eq!(
            classify_market(
                &tags(&["elections"]),
                "Who will win the 2026 Senate race in Ohio?"
            ),
            MarketCategory::Politics
        );
        assert_eq!(
            classify_market(&tags(&["pop-culture"]), "Will Taylor Swift tour Asia?"),
            MarketCategory::Other
        );
        assert_eq!(
            classify_market(&tags(&["crypto"]), "Will Polymarket launch a token?"),
            MarketCategory::Corporate
        );
    }

    #[test]
    fn tag_matching_is_case_insensitive() {
        assert_eq!(
            classify_market(&tags(&["NBA"]), "Lakers vs. Celtics"),
            MarketCategory::Sports
        );
    }
}
