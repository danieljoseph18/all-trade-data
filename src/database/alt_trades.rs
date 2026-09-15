//! `alt_trades` — per-trade rows for stonk.fun post-migration (Raydium CPMM)
//! swaps on whitelisted, non-blacklisted `alt_mints` rows.
//!
//! Twin of `amm_trades` (`trades.rs`) with the same primary key, upsert
//! semantics, batching and retention. Differences:
//!
//! - `quote_amount` / `market_cap` / reserves are in **atomic units of the
//!   row's `quote_mint_address`** (stonk.fun launches against arbitrary quote
//!   mints: SOL, DOGE, xStocks, PreStocks, …). `quote_decimals` is stored on
//!   every row so amounts can be normalised; rows with different quote mints
//!   are not directly comparable.
//! - `token_amount` is the base amount as seen by the pool (the `SwapEvent`
//!   amount, net of the Token-2022 transfer fee). `base_transfer_fee` is the
//!   fee the user paid on top (sells) or had withheld (buys), so the wallet-
//!   side amount is `token_amount + fee` for sells and `token_amount - fee`
//!   for buys.
//! - `platform` / `venue` tag the launchpad and the AMM so a second
//!   launchpad can share the table later.

use anyhow::Result;
use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use log::info;

use super::trades::{BatchInsert, coalesce_duplicate_rows};

#[derive(Debug, Clone)]
pub struct AltTradeRecord {
    pub tx_signature: String,
    /// Position of this trade within its transaction, in execution order.
    pub ix_index: i32,
    /// Launchpad tag (`stonk_fun`).
    pub platform: String,
    /// AMM tag (`raydium_cpmm`).
    pub venue: String,
    pub pool_address: String,
    pub mint_address: String,
    pub quote_mint_address: String,
    /// `swap_base_input` | `swap_base_output`.
    pub instruction_type: String,
    /// `event` (SwapEvent log), `vault_balances` (reconstructed from vault
    /// token-balance deltas when the log was truncated) or
    /// `instruction_request` (failed tx; requested bounds from the ix args).
    pub amount_source: String,
    /// True when CPMM was invoked by another program (aggregator route).
    pub is_cpi: bool,
    pub user_pubkey: String,
    pub is_buy: bool,
    /// Base amount, pool side, atomic units.
    pub token_amount: u64,
    /// Quote amount in atomic units of `quote_mint_address`.
    pub quote_amount: u64,
    /// Market cap in quote atomic units. `None` for failed trades.
    pub market_cap: Option<u64>,
    /// Post-swap pool reserves (`None` for failed trades).
    pub base_reserves: Option<u64>,
    pub quote_reserves: Option<u64>,
    /// Token-2022 transfer fee on the base leg (`None` when unknown).
    pub base_transfer_fee: Option<u64>,
    /// CPMM `trade_fee` / `creator_fee` from the event (`None` when unknown).
    pub trade_fee: Option<u64>,
    pub creator_fee: Option<u64>,
    pub base_decimals: i16,
    /// `None` until learned from `alt_mints` or a token-balance row.
    pub quote_decimals: Option<i16>,
    pub is_sol: bool,
    pub is_usdc: bool,
    pub success: bool,
    pub slot: i64,
    pub created_at: DateTime<Utc>,
    pub priority_fee: Option<i64>,
    pub transfer_tip: Option<i64>,
    pub tip_provider: Option<String>,
    pub compute_units_consumed: Option<i64>,
    pub lamports_per_compute_unit: Option<f64>,
}

/// Create the `alt_trades` table and indexes if they don't exist.
///
/// The previous `alt_trades` (bonk / bags_fm, March 2026) was dropped by
/// strategy-data's `sql/2026-09-12_create_alt_mints_stonk.sql`, so this is a
/// fresh definition; the `ALTER TABLE … ADD COLUMN IF NOT EXISTS` lines make
/// later column additions idempotent the same way `amm_trades` does.
pub async fn ensure_alt_table(pool: &Pool) -> Result<()> {
    let client = pool.get().await?;
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS alt_trades (
                tx_signature TEXT NOT NULL,
                ix_index INTEGER NOT NULL DEFAULT 0,
                platform TEXT NOT NULL DEFAULT 'stonk_fun',
                venue TEXT NOT NULL DEFAULT 'raydium_cpmm',
                pool_address TEXT NOT NULL DEFAULT '',
                mint_address TEXT NOT NULL,
                quote_mint_address TEXT NOT NULL,
                instruction_type TEXT NOT NULL DEFAULT 'unknown',
                amount_source TEXT NOT NULL DEFAULT 'event',
                is_cpi BOOLEAN NOT NULL DEFAULT FALSE,
                user_pubkey TEXT NOT NULL,
                is_buy BOOLEAN NOT NULL,
                token_amount NUMERIC(20,0) NOT NULL,
                quote_amount NUMERIC(20,0) NOT NULL,
                market_cap NUMERIC(20,0),
                base_reserves NUMERIC(20,0),
                quote_reserves NUMERIC(20,0),
                base_transfer_fee NUMERIC(20,0),
                trade_fee NUMERIC(20,0),
                creator_fee NUMERIC(20,0),
                base_decimals SMALLINT NOT NULL DEFAULT 6,
                quote_decimals SMALLINT,
                is_sol BOOLEAN NOT NULL DEFAULT FALSE,
                is_usdc BOOLEAN NOT NULL DEFAULT FALSE,
                success BOOLEAN NOT NULL DEFAULT TRUE,
                slot BIGINT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                priority_fee BIGINT,
                transfer_tip BIGINT,
                tip_provider TEXT,
                compute_units_consumed BIGINT,
                lamports_per_compute_unit DOUBLE PRECISION,
                PRIMARY KEY (tx_signature, ix_index)
            );

            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS platform TEXT NOT NULL DEFAULT 'stonk_fun';
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS venue TEXT NOT NULL DEFAULT 'raydium_cpmm';
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS base_reserves NUMERIC(20,0);
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS quote_reserves NUMERIC(20,0);
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS base_transfer_fee NUMERIC(20,0);
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS trade_fee NUMERIC(20,0);
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS creator_fee NUMERIC(20,0);
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS base_decimals SMALLINT NOT NULL DEFAULT 6;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS quote_decimals SMALLINT;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS is_sol BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS is_usdc BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS success BOOLEAN NOT NULL DEFAULT TRUE;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS priority_fee BIGINT;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS transfer_tip BIGINT;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS tip_provider TEXT;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS compute_units_consumed BIGINT;
            ALTER TABLE alt_trades ADD COLUMN IF NOT EXISTS lamports_per_compute_unit DOUBLE PRECISION;

            CREATE INDEX IF NOT EXISTS idx_alt_trades_mint ON alt_trades(mint_address);
            CREATE INDEX IF NOT EXISTS idx_alt_trades_pool ON alt_trades(pool_address);
            CREATE INDEX IF NOT EXISTS idx_alt_trades_quote_mint ON alt_trades(quote_mint_address);
            CREATE INDEX IF NOT EXISTS idx_alt_trades_slot ON alt_trades(slot);
            CREATE INDEX IF NOT EXISTS idx_alt_trades_user ON alt_trades(user_pubkey);
            CREATE INDEX IF NOT EXISTS idx_alt_trades_created_at ON alt_trades(created_at);
            CREATE INDEX IF NOT EXISTS idx_alt_trades_success ON alt_trades(success);

            COMMENT ON TABLE alt_trades IS
                'stonk.fun post-migration (Raydium CPMM) swaps on whitelisted alt_mints. quote_amount / market_cap / reserves are atomic units of quote_mint_address; token_amount is the pool-side base amount net of the Token-2022 transfer fee (base_transfer_fee).';",
        )
        .await?;

    info!("Ensured alt_trades table and indexes exist");
    Ok(())
}

/// Batch insert alt trades using a parameterized, idempotent upsert.
/// Same contract as `batch_insert_trades`: one pooled client per call,
/// duplicate keys within the batch coalesced to the latest delivery.
pub async fn batch_insert_alt_trades(pool: &Pool, trades: &[AltTradeRecord]) -> Result<()> {
    if trades.is_empty() {
        return Ok(());
    }
    let trades = coalesce_duplicate_rows(trades, |t| (t.tx_signature.as_str(), t.ix_index));
    let client = pool.get().await?;

    const COLS: usize = 32;
    let to_s = |v: u64| v.to_string();
    let opt_s = |v: Option<u64>| v.map(|x| x.to_string());
    let token_amounts: Vec<String> = trades.iter().map(|t| to_s(t.token_amount)).collect();
    let quote_amounts: Vec<String> = trades.iter().map(|t| to_s(t.quote_amount)).collect();
    let market_caps: Vec<Option<String>> = trades.iter().map(|t| opt_s(t.market_cap)).collect();
    let base_reserves: Vec<Option<String>> =
        trades.iter().map(|t| opt_s(t.base_reserves)).collect();
    let quote_reserves: Vec<Option<String>> =
        trades.iter().map(|t| opt_s(t.quote_reserves)).collect();
    let base_transfer_fees: Vec<Option<String>> =
        trades.iter().map(|t| opt_s(t.base_transfer_fee)).collect();
    let trade_fees: Vec<Option<String>> = trades.iter().map(|t| opt_s(t.trade_fee)).collect();
    let creator_fees: Vec<Option<String>> =
        trades.iter().map(|t| opt_s(t.creator_fee)).collect();

    let mut query_parts = Vec::with_capacity(trades.len());
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        Vec::with_capacity(trades.len() * COLS);

    for (i, &trade) in trades.iter().enumerate() {
        let base = i * COLS;
        let mut placeholders = Vec::with_capacity(COLS);
        for col in 1..=COLS {
            // NUMERIC columns are bound as text and cast server-side.
            let numeric = matches!(col, 13..=20);
            placeholders.push(if numeric {
                format!("${}::text::numeric", base + col)
            } else {
                format!("${}", base + col)
            });
        }
        query_parts.push(format!("({})", placeholders.join(", ")));

        params.push(&trade.tx_signature);
        params.push(&trade.ix_index);
        params.push(&trade.platform);
        params.push(&trade.venue);
        params.push(&trade.pool_address);
        params.push(&trade.mint_address);
        params.push(&trade.quote_mint_address);
        params.push(&trade.instruction_type);
        params.push(&trade.amount_source);
        params.push(&trade.is_cpi);
        params.push(&trade.user_pubkey);
        params.push(&trade.is_buy);
        params.push(&token_amounts[i]); // 13
        params.push(&quote_amounts[i]); // 14
        params.push(&market_caps[i]); // 15
        params.push(&base_reserves[i]); // 16
        params.push(&quote_reserves[i]); // 17
        params.push(&base_transfer_fees[i]); // 18
        params.push(&trade_fees[i]); // 19
        params.push(&creator_fees[i]); // 20
        params.push(&trade.base_decimals);
        params.push(&trade.quote_decimals);
        params.push(&trade.is_sol);
        params.push(&trade.is_usdc);
        params.push(&trade.success);
        params.push(&trade.slot);
        params.push(&trade.created_at);
        params.push(&trade.priority_fee);
        params.push(&trade.transfer_tip);
        params.push(&trade.tip_provider);
        params.push(&trade.compute_units_consumed);
        params.push(&trade.lamports_per_compute_unit);
    }
    debug_assert_eq!(params.len(), trades.len() * COLS);

    let query = format!(
        "INSERT INTO alt_trades (tx_signature, ix_index, platform, venue, pool_address, mint_address, \
         quote_mint_address, instruction_type, amount_source, is_cpi, user_pubkey, is_buy, \
         token_amount, quote_amount, market_cap, base_reserves, quote_reserves, base_transfer_fee, \
         trade_fee, creator_fee, base_decimals, quote_decimals, is_sol, is_usdc, success, slot, \
         created_at, priority_fee, transfer_tip, tip_provider, compute_units_consumed, \
         lamports_per_compute_unit) VALUES {} \
         ON CONFLICT (tx_signature, ix_index) DO UPDATE SET \
         platform = EXCLUDED.platform, venue = EXCLUDED.venue, \
         pool_address = EXCLUDED.pool_address, mint_address = EXCLUDED.mint_address, \
         quote_mint_address = EXCLUDED.quote_mint_address, instruction_type = EXCLUDED.instruction_type, \
         amount_source = EXCLUDED.amount_source, is_cpi = EXCLUDED.is_cpi, user_pubkey = EXCLUDED.user_pubkey, \
         is_buy = EXCLUDED.is_buy, token_amount = EXCLUDED.token_amount, quote_amount = EXCLUDED.quote_amount, \
         market_cap = EXCLUDED.market_cap, base_reserves = EXCLUDED.base_reserves, \
         quote_reserves = EXCLUDED.quote_reserves, base_transfer_fee = EXCLUDED.base_transfer_fee, \
         trade_fee = EXCLUDED.trade_fee, creator_fee = EXCLUDED.creator_fee, \
         base_decimals = EXCLUDED.base_decimals, quote_decimals = COALESCE(EXCLUDED.quote_decimals, alt_trades.quote_decimals), \
         is_sol = EXCLUDED.is_sol, is_usdc = EXCLUDED.is_usdc, success = EXCLUDED.success, slot = EXCLUDED.slot, \
         priority_fee = EXCLUDED.priority_fee, transfer_tip = EXCLUDED.transfer_tip, \
         tip_provider = EXCLUDED.tip_provider, compute_units_consumed = EXCLUDED.compute_units_consumed, \
         lamports_per_compute_unit = EXCLUDED.lamports_per_compute_unit",
        query_parts.join(",")
    );

    client.execute(&query, &params).await?;
    Ok(())
}

impl BatchInsert for AltTradeRecord {
    const LABEL: &'static str = "alt_trades";
    // 500 * 32 = 16,000 bind parameters per statement, under the 65,535 cap.
    const CHUNK: usize = 500;

    async fn insert_batch(pool: &Pool, rows: &[Self]) -> Result<()> {
        batch_insert_alt_trades(pool, rows).await
    }
}
