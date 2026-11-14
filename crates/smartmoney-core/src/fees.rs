//! Polymarket taker fees and shared question heuristics.

use crate::types::MarketCategory;

/// Taker fee rate per category (docs.polymarket.com/trading/fees.md,
/// checked 2026-11-10). Geopolitics is fee-free but cannot be told apart
/// from politics here, so the higher rate is used (conservative).
pub fn fee_rate(category: MarketCategory) -> f64 {
    match category {
        MarketCategory::CryptoPrice => 0.07,
        MarketCategory::Sports | MarketCategory::Other => 0.05,
        MarketCategory::Politics
        | MarketCategory::Personnel
        | MarketCategory::Corporate
        | MarketCategory::Legal => 0.04,
    }
}

/// Taker fee in USDC: `shares x rate x p x (1 - p)`.
pub fn taker_fee_usd(shares: f64, price: f64, rate: f64) -> f64 {
    shares * rate * price * (1.0 - price)
}

/// Question patterns of deadline markets ("Will X happen by <date>?"),
/// where NO is the status quo.
const DEADLINE_PATTERNS: &[&str] = &[
    " by ",
    " before ",
    "by end of",
    "by the end of",
    " in 2025",
    " in 2026",
    " in 2027",
    "this year",
    "this month",
    "this week",
];

/// Whether a market question is deadline-style.
pub fn is_deadline_question(question: &str) -> bool {
    let q = question.to_lowercase();
    q.starts_with("will ") && DEADLINE_PATTERNS.iter().any(|p| q.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_fee() {
        assert_eq!(fee_rate(MarketCategory::Sports), 0.05);
        assert_eq!(fee_rate(MarketCategory::CryptoPrice), 0.07);
        assert_eq!(fee_rate(MarketCategory::Politics), 0.04);
        // 100 shares at 50¢, 5 %: 100 x 0.05 x 0.25 = 1.25.
        assert!((taker_fee_usd(100.0, 0.5, 0.05) - 1.25).abs() < 1e-12);
        // Fees vanish toward the extremes.
        assert!(taker_fee_usd(100.0, 0.98, 0.05) < 0.1);
    }

    #[test]
    fn deadline_questions() {
        assert!(is_deadline_question("Will X resign by March 31?"));
        assert!(is_deadline_question("Will Y happen before 2027?"));
        assert!(!is_deadline_question("Will the Lakers win tonight?"));
        assert!(!is_deadline_question(
            "Highest temperature in NYC on May 1?"
        ));
    }
}
