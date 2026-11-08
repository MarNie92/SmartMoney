-- SmartMoney initial schema.
-- Timestamps are unix epoch seconds (INTEGER) unless stated otherwise.
-- Money values are USDC (REAL); prices are outcome-token prices in 0..1.
-- raw_json columns keep the original API payload for forward compatibility.

-- Polymarket markets (Gamma API), one row per condition.
CREATE TABLE markets (
    condition_id           TEXT PRIMARY KEY,
    gamma_id               TEXT,
    question               TEXT,
    slug                   TEXT,
    event_slug             TEXT,
    category               TEXT,            -- SmartMoney MarketCategory
    gamma_category         TEXT,            -- category as delivered by Gamma
    tags_json              TEXT,
    volume_usd             REAL,
    volume_24h_usd         REAL,
    end_date               INTEGER,
    closed                 INTEGER NOT NULL DEFAULT 0,
    outcomes_json          TEXT,
    clob_token_ids_json    TEXT,
    resolution_status      TEXT,
    resolved_outcome_index INTEGER,         -- NULL until resolved
    resolved_at            INTEGER,
    raw_json               TEXT,
    fetched_at             INTEGER NOT NULL
);
CREATE INDEX idx_markets_closed ON markets (closed);

-- Trades above the collector threshold (Data API /v2/trades).
CREATE TABLE trades (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_hash TEXT NOT NULL,
    proxy_wallet     TEXT NOT NULL,
    condition_id     TEXT NOT NULL,
    token_id         TEXT NOT NULL,
    side             TEXT NOT NULL CHECK (side IN ('BUY', 'SELL')),
    outcome          TEXT,
    outcome_index    INTEGER,
    size             REAL NOT NULL,         -- shares
    price            REAL NOT NULL,         -- USDC per share
    usdc_value       REAL NOT NULL,         -- size * price
    timestamp        INTEGER NOT NULL,
    title            TEXT,
    slug             TEXT,
    event_slug       TEXT,
    raw_json         TEXT,
    ingested_at      INTEGER NOT NULL,
    UNIQUE (transaction_hash, proxy_wallet, token_id, side, size, price)
);
CREATE INDEX idx_trades_wallet ON trades (proxy_wallet, timestamp);
CREATE INDEX idx_trades_condition ON trades (condition_id, timestamp);
CREATE INDEX idx_trades_timestamp ON trades (timestamp);

-- Enriched wallet data (user-stats, activity, positions).
CREATE TABLE wallets (
    address              TEXT PRIMARY KEY,
    name                 TEXT,
    pseudonym            TEXT,
    join_date            INTEGER,           -- user-stats join_date, may be NULL
    first_activity_at    INTEGER,           -- earliest activity row seen
    markets_traded       INTEGER,           -- distinct markets (user-stats trades)
    total_positions_usd  REAL,
    largest_position_usd REAL,
    raw_stats_json       TEXT,
    enriched_at          INTEGER
);

-- Latest known positions per wallet, for the concentration score.
CREATE TABLE wallet_positions (
    address       TEXT NOT NULL,
    token_id      TEXT NOT NULL,
    condition_id  TEXT NOT NULL,
    current_size  REAL,
    current_value REAL,
    total_cost    REAL,
    fetched_at    INTEGER NOT NULL,
    PRIMARY KEY (address, token_id)
);

-- One score per trade, with full breakdown.
CREATE TABLE scores (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    trade_id         INTEGER NOT NULL UNIQUE REFERENCES trades (id) ON DELETE CASCADE,
    proxy_wallet     TEXT NOT NULL,
    condition_id     TEXT NOT NULL,
    total            REAL NOT NULL,
    wallet_freshness REAL NOT NULL,
    price_asymmetry  REAL NOT NULL,
    relative_size    REAL NOT NULL,
    concentration    REAL NOT NULL,
    market_type      REAL NOT NULL,
    cluster_bonus    REAL NOT NULL,
    details_json     TEXT NOT NULL,         -- inputs and reasoning per component
    scorer_version   INTEGER NOT NULL,
    created_at       INTEGER NOT NULL
);
CREATE INDEX idx_scores_total ON scores (total, created_at);
CREATE INDEX idx_scores_wallet ON scores (proxy_wallet);

-- Telegram alerts that were sent.
CREATE TABLE alerts (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    score_id            INTEGER NOT NULL REFERENCES scores (id) ON DELETE CASCADE,
    chat_id             INTEGER NOT NULL,
    telegram_message_id INTEGER,
    status              TEXT NOT NULL DEFAULT 'sent',  -- sent | ignored | followed
    sent_at             INTEGER NOT NULL,
    UNIQUE (score_id, chat_id)
);

CREATE TABLE watchlist (
    address  TEXT PRIMARY KEY,
    label    TEXT,
    added_at INTEGER NOT NULL
);

-- Order book snapshots (CLOB /book), used for alerts and simulated fills.
CREATE TABLE orderbook_snapshots (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id  TEXT NOT NULL,
    taken_at  INTEGER NOT NULL,
    best_bid  REAL,
    best_ask  REAL,
    bids_json TEXT NOT NULL,
    asks_json TEXT NOT NULL
);
CREATE INDEX idx_orderbook_token ON orderbook_snapshots (token_id, taken_at);

-- Price history points (CLOB /prices-history), used by the backtest.
CREATE TABLE price_history (
    token_id TEXT NOT NULL,
    ts       INTEGER NOT NULL,
    price    REAL NOT NULL,
    PRIMARY KEY (token_id, ts)
);

-- Paper-trading accounts, one per strategy.
CREATE TABLE sim_accounts (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy         TEXT NOT NULL UNIQUE,
    config_json      TEXT NOT NULL,
    starting_balance REAL NOT NULL,
    cash             REAL NOT NULL,
    created_at       INTEGER NOT NULL,
    reset_at         INTEGER
);

CREATE TABLE sim_positions (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id          INTEGER NOT NULL REFERENCES sim_accounts (id) ON DELETE CASCADE,
    score_id            INTEGER REFERENCES scores (id) ON DELETE SET NULL,
    condition_id        TEXT NOT NULL,
    token_id            TEXT NOT NULL,
    outcome_index       INTEGER,
    origin              TEXT NOT NULL CHECK (origin IN ('auto', 'manual')),
    shares              REAL NOT NULL,
    avg_price           REAL NOT NULL,
    cost_usd            REAL NOT NULL,      -- including fees
    fees_usd            REAL NOT NULL,
    insider_price       REAL,               -- price of the triggering trade
    status              TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'settled')),
    mark_price          REAL,
    marked_at           INTEGER,
    payout_usd          REAL,
    realized_pnl        REAL,
    opened_at           INTEGER NOT NULL,
    settled_at          INTEGER
);
CREATE INDEX idx_sim_positions_account ON sim_positions (account_id, status);
CREATE INDEX idx_sim_positions_condition ON sim_positions (condition_id, status);

CREATE TABLE sim_fills (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    position_id  INTEGER NOT NULL REFERENCES sim_positions (id) ON DELETE CASCADE,
    shares       REAL NOT NULL,
    price        REAL NOT NULL,
    fee_usd      REAL NOT NULL,
    decided_at   INTEGER NOT NULL,
    filled_at    INTEGER NOT NULL,
    snapshot_id  INTEGER REFERENCES orderbook_snapshots (id) ON DELETE SET NULL
);

CREATE TABLE sim_equity (
    account_id INTEGER NOT NULL REFERENCES sim_accounts (id) ON DELETE CASCADE,
    ts         INTEGER NOT NULL,
    equity     REAL NOT NULL,
    PRIMARY KEY (account_id, ts)
);

CREATE TABLE backtest_runs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at   INTEGER NOT NULL,
    params_json  TEXT NOT NULL,
    results_json TEXT NOT NULL
);

-- Runtime settings changed at runtime (e.g. /threshold) and collector cursors.
CREATE TABLE kv_state (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
