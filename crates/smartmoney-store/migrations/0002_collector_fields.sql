-- Fields discovered while verifying the live APIs (2026-11-08).

-- Gamma outcomePrices (JSON array of numbers) and negRisk flag.
ALTER TABLE markets ADD COLUMN outcome_prices_json TEXT;
ALTER TABLE markets ADD COLUMN neg_risk INTEGER;
-- /v2/resolutions payouts converted to USDC per share (JSON array, e.g. [0, 1]).
ALTER TABLE markets ADD COLUMN payouts_json TEXT;

-- /v2/user-stats all_time_pnl.trade_count (number of fills, all markets).
ALTER TABLE wallets ADD COLUMN trade_count INTEGER;
