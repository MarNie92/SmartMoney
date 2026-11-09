//! Configuration: non-secret settings come from a TOML file, secrets come
//! only from environment variables.
//!
//! Resolution order:
//! 1. Built-in defaults (`Config::default()`).
//! 2. TOML file: `$SMARTMONEY_CONFIG`, else `config.local.toml` if present.
//! 3. Environment overrides (`SMARTMONEY_*`), including all secrets.

use crate::secret::Secret;
use crate::types::MarketCategory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const ENV_CONFIG_PATH: &str = "SMARTMONEY_CONFIG";
pub const ENV_DATABASE_PATH: &str = "SMARTMONEY_DATABASE_PATH";
pub const ENV_TELEGRAM_TOKEN: &str = "SMARTMONEY_TELEGRAM_BOT_TOKEN";
pub const ENV_TELEGRAM_CHAT_IDS: &str = "SMARTMONEY_TELEGRAM_ALLOWED_CHAT_IDS";
pub const ENV_DASHBOARD_TOKEN: &str = "SMARTMONEY_DASHBOARD_TOKEN";
pub const ENV_DASHBOARD_BIND: &str = "SMARTMONEY_DASHBOARD_BIND";
pub const DEFAULT_LOCAL_CONFIG: &str = "config.local.toml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid value for {name}: {message}")]
    InvalidEnv { name: String, message: String },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub database: DatabaseConfig,
    pub collector: CollectorConfig,
    pub scoring: ScoringConfig,
    pub telegram: TelegramConfig,
    pub dashboard: DashboardConfig,
    pub simulation: SimulationConfig,
    pub backtest: BacktestConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("smartmoney.db"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CollectorConfig {
    pub gamma_api_url: String,
    pub data_api_url: String,
    pub clob_api_url: String,
    /// Only trades with a USDC value (size * price) at or above this are kept.
    pub minimum_trade_usd: f64,
    pub poll_interval_seconds: u64,
    /// Client-side request budget per 10 seconds (all APIs combined).
    pub max_requests_per_10s: u32,
    pub backoff_initial_ms: u64,
    pub backoff_max_ms: u64,
    pub request_timeout_seconds: u64,
    /// Wallet history and market data older than this are re-fetched.
    pub enrich_cache_minutes: u64,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            gamma_api_url: "https://gamma-api.polymarket.com".into(),
            data_api_url: "https://data-api.polymarket.com".into(),
            clob_api_url: "https://clob.polymarket.com".into(),
            minimum_trade_usd: 2000.0,
            poll_interval_seconds: 30,
            max_requests_per_10s: 50,
            backoff_initial_ms: 500,
            backoff_max_ms: 60_000,
            request_timeout_seconds: 20,
            enrich_cache_minutes: 60,
        }
    }
}

/// How the market-type component is combined with the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreMode {
    /// All components are added (market type is one of them).
    #[default]
    Additive,
    /// The other components form a 0-100 base that is multiplied by the
    /// category factor; concentration only counts for wallets with history.
    Multiplicative,
}

/// Maximum points per score component. The sum of all components except the
/// cluster bonus forms the 0-100 base score; the final score is capped at 100.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScoreWeights {
    pub wallet_freshness: f64,
    pub price_asymmetry: f64,
    pub relative_size: f64,
    pub concentration: f64,
    pub market_type: f64,
    pub cluster_bonus: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            wallet_freshness: 25.0,
            price_asymmetry: 25.0,
            relative_size: 20.0,
            concentration: 15.0,
            market_type: 15.0,
            cluster_bonus: 10.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScoringConfig {
    /// Alerts are sent for scores at or above this value.
    pub alert_threshold: f64,
    /// How the market type enters the score.
    pub mode: ScoreMode,
    pub weights: ScoreWeights,
    /// Wallet freshness: full points below this age, fading to 0 at
    /// `fresh_zero_age_days`.
    pub fresh_full_age_days: f64,
    pub fresh_zero_age_days: f64,
    /// Wallet freshness: full points with fewer distinct markets traded than
    /// this, fading to 0 at `fresh_zero_markets`.
    pub fresh_full_markets: u32,
    pub fresh_zero_markets: u32,
    /// Price asymmetry: full points at or below this entry price, fading to 0
    /// at `asymmetry_zero_price`.
    pub asymmetry_full_price: f64,
    pub asymmetry_zero_price: f64,
    /// Share of the 24h market volume that earns the full relative-size score.
    pub relative_size_full_share: f64,
    /// Concentration: share of the wallet's position value in this market;
    /// 0 points at or below `concentration_zero_share`, full at or above
    /// `concentration_full_share`.
    pub concentration_zero_share: f64,
    pub concentration_full_share: f64,
    /// Cluster bonus: at least `cluster_min_wallets` fresh wallets (including
    /// this one) bought the same outcome within this window.
    pub cluster_window_minutes: u64,
    pub cluster_min_wallets: u32,
    /// Fraction (0-1) of `market_type` points per category.
    pub category_factors: BTreeMap<MarketCategory, f64>,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        let category_factors = BTreeMap::from([
            (MarketCategory::Personnel, 1.0),
            (MarketCategory::Corporate, 1.0),
            (MarketCategory::Legal, 1.0),
            (MarketCategory::Politics, 0.6),
            (MarketCategory::Other, 0.5),
            (MarketCategory::Sports, 0.1),
            (MarketCategory::CryptoPrice, 0.0),
        ]);
        Self {
            alert_threshold: 60.0,
            mode: ScoreMode::Additive,
            weights: ScoreWeights::default(),
            fresh_full_age_days: 7.0,
            fresh_zero_age_days: 30.0,
            fresh_full_markets: 5,
            fresh_zero_markets: 20,
            asymmetry_full_price: 0.05,
            asymmetry_zero_price: 0.20,
            relative_size_full_share: 0.10,
            concentration_zero_share: 0.20,
            concentration_full_share: 0.80,
            cluster_window_minutes: 30,
            cluster_min_wallets: 2,
            category_factors,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelegramConfig {
    /// Only these chat IDs may use the bot. Also settable via
    /// `SMARTMONEY_TELEGRAM_ALLOWED_CHAT_IDS` (comma-separated).
    pub allowed_chat_ids: Vec<i64>,
    /// Secret: only from `SMARTMONEY_TELEGRAM_BOT_TOKEN`.
    #[serde(skip)]
    pub bot_token: Option<Secret>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardConfig {
    /// Keep on localhost or a Tailscale address; never a public interface.
    pub bind_address: String,
    /// Secret: only from `SMARTMONEY_DASHBOARD_TOKEN`.
    #[serde(skip)]
    pub token: Option<Secret>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".into(),
            token: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum StakeRule {
    /// Fixed USDC amount per trade.
    Fixed(f64),
    /// Percentage (0-100) of the strategy's current cash.
    PercentOfCash(f64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyConfig {
    pub name: String,
    /// Auto mode follows alerts with a score at or above this value.
    pub min_score: f64,
    pub stake: StakeRule,
    pub max_stake_per_market_usd: f64,
    /// Skip the entry if the price rose more than this percentage since the
    /// insider trade.
    pub max_price_move_pct: f64,
    #[serde(default = "default_true")]
    pub auto_follow: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimulationConfig {
    pub starting_balance_usd: f64,
    /// Delay between decision and the simulated fill.
    pub execution_delay_seconds: u64,
    /// Fee in basis points of the filled notional.
    pub fee_bps: f64,
    /// Strategy used for manual follows via the Telegram buttons.
    pub manual_strategy: String,
    pub strategies: Vec<StrategyConfig>,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        let strategy = |name: &str, min_score: f64| StrategyConfig {
            name: name.into(),
            min_score,
            stake: StakeRule::Fixed(25.0),
            max_stake_per_market_usd: 100.0,
            max_price_move_pct: 30.0,
            auto_follow: true,
        };
        Self {
            starting_balance_usd: 1000.0,
            execution_delay_seconds: 30,
            fee_bps: 0.0,
            manual_strategy: "manual".into(),
            strategies: vec![
                strategy("score60", 60.0),
                strategy("score70", 70.0),
                strategy("score80", 80.0),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BacktestConfig {
    /// Follow delays (minutes after the insider trade) to evaluate.
    pub follow_delays_minutes: Vec<u64>,
    /// Upper bounds of the score buckets, e.g. [40, 60, 80, 100].
    pub score_buckets: Vec<f64>,
    pub stake_usd: f64,
    /// Price penalty on follow entries (basis points of the price).
    pub slippage_bps: f64,
    /// Gamma tag slugs of niche markets to sample.
    pub niche_tags: Vec<String>,
    /// Gamma tag slugs of comparison markets (low insider value).
    pub control_tags: Vec<String>,
    pub markets_per_niche_tag: usize,
    pub markets_per_control_tag: usize,
    pub min_market_volume_usd: f64,
    /// Markets with more trades are skipped (keeps fetches bounded).
    pub max_trades_per_market: usize,
    /// Only markets that ended within this many days.
    pub lookback_days: u64,
    /// Smallest trade fetched as a candidate (USDC).
    pub min_candidate_usd: f64,
    /// Payout-filter variant: also keep trades whose potential payout
    /// (shares x $1) reaches this value.
    pub variant_min_payout_usd: f64,
    /// Wallet trade history window used to approximate concentration.
    pub wallet_history_days: u64,
    pub requests_per_10s: u32,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            follow_delays_minutes: vec![0, 5, 15, 60],
            score_buckets: vec![20.0, 40.0, 60.0, 80.0, 100.0],
            stake_usd: 100.0,
            slippage_bps: 50.0,
            niche_tags: [
                "politics",
                "business",
                "tech",
                "courts",
                "world",
                "economy",
                "ai",
                "pop-culture",
            ]
            .map(String::from)
            .to_vec(),
            control_tags: ["sports", "crypto"].map(String::from).to_vec(),
            markets_per_niche_tag: 60,
            markets_per_control_tag: 40,
            min_market_volume_usd: 5000.0,
            max_trades_per_market: 20_000,
            lookback_days: 180,
            min_candidate_usd: 500.0,
            variant_min_payout_usd: 15_000.0,
            wallet_history_days: 30,
            requests_per_10s: 100,
        }
    }
}

impl Config {
    /// Load from the default locations and apply process environment overrides.
    pub fn load() -> Result<Self, ConfigError> {
        let path = std::env::var_os(ENV_CONFIG_PATH)
            .map(PathBuf::from)
            .or_else(|| {
                let local = PathBuf::from(DEFAULT_LOCAL_CONFIG);
                local.exists().then_some(local)
            });
        let mut config = match path {
            Some(path) => Self::from_file(&path)?,
            None => Self::default(),
        };
        config.apply_env(|name| std::env::var(name).ok())?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Apply `SMARTMONEY_*` overrides. `lookup` abstracts the environment so
    /// tests do not need to mutate process state.
    pub fn apply_env(
        &mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<(), ConfigError> {
        let get = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());

        if let Some(path) = get(ENV_DATABASE_PATH) {
            self.database.path = PathBuf::from(path);
        }
        if let Some(token) = get(ENV_TELEGRAM_TOKEN) {
            self.telegram.bot_token = Some(Secret::new(token.trim()));
        }
        if let Some(ids) = get(ENV_TELEGRAM_CHAT_IDS) {
            self.telegram.allowed_chat_ids = parse_chat_ids(&ids)?;
        }
        if let Some(token) = get(ENV_DASHBOARD_TOKEN) {
            self.dashboard.token = Some(Secret::new(token.trim()));
        }
        if let Some(bind) = get(ENV_DASHBOARD_BIND) {
            self.dashboard.bind_address = bind.trim().to_owned();
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: &str| Err(ConfigError::Invalid(msg.to_owned()));
        if self.collector.minimum_trade_usd < 0.0 {
            return invalid("collector.minimum_trade_usd must be >= 0");
        }
        if self.collector.poll_interval_seconds == 0 {
            return invalid("collector.poll_interval_seconds must be > 0");
        }
        if self.collector.max_requests_per_10s == 0 {
            return invalid("collector.max_requests_per_10s must be > 0");
        }
        if !(0.0..=100.0).contains(&self.scoring.alert_threshold) {
            return invalid("scoring.alert_threshold must be within 0..=100");
        }
        let s = &self.scoring;
        if !(0.0 <= s.asymmetry_full_price
            && s.asymmetry_full_price < s.asymmetry_zero_price
            && s.asymmetry_zero_price <= 1.0)
        {
            return invalid("scoring: need 0 <= asymmetry_full_price < asymmetry_zero_price <= 1");
        }
        if !(0.0 <= s.fresh_full_age_days && s.fresh_full_age_days < s.fresh_zero_age_days) {
            return invalid("scoring: need 0 <= fresh_full_age_days < fresh_zero_age_days");
        }
        if s.fresh_full_markets >= s.fresh_zero_markets {
            return invalid("scoring: need fresh_full_markets < fresh_zero_markets");
        }
        if !(0.0 <= s.concentration_zero_share
            && s.concentration_zero_share < s.concentration_full_share
            && s.concentration_full_share <= 1.0)
        {
            return invalid(
                "scoring: need 0 <= concentration_zero_share < concentration_full_share <= 1",
            );
        }
        if s.relative_size_full_share <= 0.0 {
            return invalid("scoring.relative_size_full_share must be > 0");
        }
        let w = &s.weights;
        if [
            w.wallet_freshness,
            w.price_asymmetry,
            w.relative_size,
            w.concentration,
            w.market_type,
            w.cluster_bonus,
        ]
        .iter()
        .any(|v| *v < 0.0)
        {
            return invalid("scoring.weights must be >= 0");
        }
        if self
            .scoring
            .category_factors
            .values()
            .any(|f| !(0.0..=1.0).contains(f))
        {
            return invalid("scoring.category_factors values must be within 0..=1");
        }
        if self.simulation.starting_balance_usd <= 0.0 {
            return invalid("simulation.starting_balance_usd must be > 0");
        }
        let mut names = std::collections::BTreeSet::new();
        for s in &self.simulation.strategies {
            if !names.insert(s.name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate strategy name: {}",
                    s.name
                )));
            }
            match s.stake {
                StakeRule::Fixed(v) if v <= 0.0 => {
                    return invalid("fixed stake must be > 0");
                }
                StakeRule::PercentOfCash(p) if !(0.0..=100.0).contains(&p) || p == 0.0 => {
                    return invalid("percent stake must be within (0, 100]");
                }
                _ => {}
            }
        }
        if names.contains(self.simulation.manual_strategy.as_str()) {
            return invalid("simulation.manual_strategy must not reuse an auto strategy name");
        }
        Ok(())
    }
}

fn parse_chat_ids(raw: &str) -> Result<Vec<i64>, ConfigError> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>().map_err(|e| ConfigError::InvalidEnv {
                name: ENV_TELEGRAM_CHAT_IDS.into(),
                message: format!("{s:?}: {e}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn defaults_are_valid() {
        let config = Config::default();
        config.validate().unwrap();
        assert_eq!(config.collector.minimum_trade_usd, 2000.0);
        assert_eq!(config.simulation.strategies.len(), 3);
        assert!(config.telegram.bot_token.is_none());
    }

    #[test]
    fn example_config_parses_and_validates() {
        let text = include_str!("../../../config.example.toml");
        let config = Config::from_toml_str(text).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn partial_toml_keeps_defaults() {
        let config = Config::from_toml_str(
            r#"
            [collector]
            minimum_trade_usd = 5000.0

            [scoring.weights]
            price_asymmetry = 30.0
            "#,
        )
        .unwrap();
        assert_eq!(config.collector.minimum_trade_usd, 5000.0);
        assert_eq!(config.collector.poll_interval_seconds, 30);
        assert_eq!(config.scoring.weights.price_asymmetry, 30.0);
        assert_eq!(config.scoring.weights.wallet_freshness, 25.0);
    }

    #[test]
    fn secrets_are_rejected_in_toml() {
        let result = Config::from_toml_str(
            r#"
            [telegram]
            bot_token = "123:abc"
            "#,
        );
        assert!(result.is_err(), "tokens must only come from env vars");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::from_toml_str("[collector]\nminimum_trade = 1.0").is_err());
    }

    #[test]
    fn env_overrides_apply() {
        let mut config = Config::default();
        config
            .apply_env(env(&[
                (ENV_TELEGRAM_TOKEN, " 123:abc "),
                (ENV_TELEGRAM_CHAT_IDS, "42, -1001234"),
                (ENV_DASHBOARD_TOKEN, "dash"),
                (ENV_DATABASE_PATH, "/var/lib/smartmoney/db.sqlite"),
                (ENV_DASHBOARD_BIND, "100.64.0.1:8080"),
            ]))
            .unwrap();
        assert_eq!(config.telegram.bot_token.unwrap().expose(), "123:abc");
        assert_eq!(config.telegram.allowed_chat_ids, vec![42, -1001234]);
        assert_eq!(config.dashboard.token.unwrap().expose(), "dash");
        assert_eq!(
            config.database.path,
            PathBuf::from("/var/lib/smartmoney/db.sqlite")
        );
        assert_eq!(config.dashboard.bind_address, "100.64.0.1:8080");
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let mut config = Config::default();
        config
            .apply_env(env(&[(ENV_TELEGRAM_TOKEN, "  ")]))
            .unwrap();
        assert!(config.telegram.bot_token.is_none());
    }

    #[test]
    fn invalid_chat_ids_fail() {
        let mut config = Config::default();
        let err = config
            .apply_env(env(&[(ENV_TELEGRAM_CHAT_IDS, "42,abc")]))
            .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnv { .. }));
    }

    #[test]
    fn debug_output_hides_tokens() {
        let mut config = Config::default();
        config
            .apply_env(env(&[(ENV_TELEGRAM_TOKEN, "supersecret")]))
            .unwrap();
        assert!(!format!("{config:?}").contains("supersecret"));
    }

    #[test]
    fn validation_catches_bad_values() {
        let mut config = Config::default();
        config.scoring.alert_threshold = 150.0;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.simulation.strategies[1].name = "score60".into();
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.simulation.strategies[0].stake = StakeRule::PercentOfCash(0.0);
        assert!(config.validate().is_err());
    }
}
