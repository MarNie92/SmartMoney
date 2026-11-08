use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Trade direction from the trading wallet's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Side {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "BUY" => Ok(Side::Buy),
            "SELL" => Ok(Side::Sell),
            other => Err(format!("unknown side: {other}")),
        }
    }
}

/// Coarse market classification used by the market-type score component.
/// Niche markets (personnel decisions, companies, court cases) are more
/// susceptible to insider knowledge than sports or crypto price markets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketCategory {
    Personnel,
    Corporate,
    Legal,
    Politics,
    Other,
    Sports,
    CryptoPrice,
}

impl MarketCategory {
    pub const ALL: [MarketCategory; 7] = [
        MarketCategory::Personnel,
        MarketCategory::Corporate,
        MarketCategory::Legal,
        MarketCategory::Politics,
        MarketCategory::Other,
        MarketCategory::Sports,
        MarketCategory::CryptoPrice,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MarketCategory::Personnel => "personnel",
            MarketCategory::Corporate => "corporate",
            MarketCategory::Legal => "legal",
            MarketCategory::Politics => "politics",
            MarketCategory::Other => "other",
            MarketCategory::Sports => "sports",
            MarketCategory::CryptoPrice => "crypto_price",
        }
    }
}

impl fmt::Display for MarketCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MarketCategory {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MarketCategory::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| format!("unknown market category: {s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_round_trips() {
        assert_eq!("buy".parse::<Side>().unwrap(), Side::Buy);
        assert_eq!(" SELL ".parse::<Side>().unwrap(), Side::Sell);
        assert!("hold".parse::<Side>().is_err());
        assert_eq!(Side::Buy.to_string(), "BUY");
    }

    #[test]
    fn category_round_trips() {
        for c in MarketCategory::ALL {
            assert_eq!(c.as_str().parse::<MarketCategory>().unwrap(), c);
        }
    }
}
