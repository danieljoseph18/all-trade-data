use anyhow::Result;
use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use log::{info, warn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

/// A single AMM trade record to be inserted into the database.
///
/// `ix_index` is the position of this trade within its transaction. A single tx
/// can contain multiple trade instructions (arbitrage, MEV) — without this
/// disambiguator, an `ON CONFLICT (tx_signature)` insert would silently drop
/// all but the first trade.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub tx_signature: String,
    pub ix_index: i32,
    pub pool_address: String,
    pub mint_address: String,
    pub quote_mint_address: String,
    pub instruction_type: String,
    /// `event` is executed volume; `instruction_request` is only a submitted bound.
    pub amount_source: String,
    /// True when PumpSwap was invoked by another on-chain program rather than
    /// as a top-level transaction instruction.
    pub is_cpi: bool,
    pub user_pubkey: String,
    pub is_buy: bool,
    pub token_amount: u64,
    /// Quote amount in native pool units: lamports for SOL pools, USDC
    /// microunits for USDC pools (`is_usdc = true`).
    pub sol_amount: u64,
    /// Market cap in native pool quote units. `None` for failed trades, which
    /// carry no executed event payload to derive reserves/price from.
    pub market_cap: Option<u64>,
    pub is_usdc: bool,
    /// Whether the transaction landed. `false` rows are attempted trades that
    /// failed on-chain; their `token_amount`/`sol_amount` are the *requested*
    /// amounts from the instruction args, not executed amounts.
    pub success: bool,
    pub slot: i64,
    pub created_at: DateTime<Utc>,
    pub priority_fee: Option<i64>,
    pub transfer_tip: Option<i64>,
    pub tip_provider: Option<String>,
    /// Compute units actually consumed by the transaction (`meta.compute_units_consumed`).
    pub compute_units_consumed: Option<i64>,
    /// Priority fee bid expressed as lamports per compute unit
    /// (`priority_fee` microlamports/CU ÷ 1_000_000).
    pub lamports_per_compute_unit: Option<f64>,
}

/// Create the amm_trades table and indexes if they don't exist, and migrate
/// pre-existing schema (tx_signature-only PK) to the composite PK form.
pub async fn ensure_table(pool: &Pool) -> Result<()> {
    let client = pool.get().await?;

    // Idempotent create + migration. The DO block widens an existing single-
    // column PK (tx_signature) to the composite (tx_signature, ix_index) form,
    // which is required to record every trade in multi-trade txs.
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS amm_trades (
                tx_signature TEXT NOT NULL,
                ix_index INTEGER NOT NULL DEFAULT 0,
                pool_address TEXT NOT NULL DEFAULT '',
                mint_address TEXT NOT NULL,
                quote_mint_address TEXT NOT NULL DEFAULT 'So11111111111111111111111111111111111111112',
                instruction_type TEXT NOT NULL DEFAULT 'unknown',
                amount_source TEXT NOT NULL DEFAULT 'event',
                is_cpi BOOLEAN NOT NULL DEFAULT FALSE,
                user_pubkey TEXT NOT NULL,
                is_buy BOOLEAN NOT NULL,
                token_amount NUMERIC(20,0) NOT NULL,
                sol_amount NUMERIC(20,0) NOT NULL,
                market_cap NUMERIC(20,0),
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

            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS ix_index INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS pool_address TEXT NOT NULL DEFAULT '';
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS quote_mint_address TEXT NOT NULL DEFAULT 'So11111111111111111111111111111111111111112';
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS instruction_type TEXT NOT NULL DEFAULT 'unknown';
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS amount_source TEXT NOT NULL DEFAULT 'event';
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS is_cpi BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS priority_fee BIGINT;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS transfer_tip BIGINT;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS tip_provider TEXT;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS is_usdc BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS success BOOLEAN NOT NULL DEFAULT TRUE;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS compute_units_consumed BIGINT;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS lamports_per_compute_unit DOUBLE PRECISION;
            DO $$
            BEGIN
                IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                    WHERE table_schema = current_schema() AND table_name = 'amm_trades'
                      AND column_name = 'token_amount'
                      AND data_type <> 'numeric'
                ) THEN
                    ALTER TABLE amm_trades ALTER COLUMN token_amount TYPE NUMERIC(20,0) USING token_amount::numeric;
                END IF;
                IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                    WHERE table_schema = current_schema() AND table_name = 'amm_trades'
                      AND column_name = 'sol_amount' AND data_type <> 'numeric'
                ) THEN
                    ALTER TABLE amm_trades ALTER COLUMN sol_amount TYPE NUMERIC(20,0) USING sol_amount::numeric;
                END IF;
                IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                    WHERE table_schema = current_schema() AND table_name = 'amm_trades'
                      AND column_name = 'market_cap' AND data_type <> 'numeric'
                ) THEN
                    ALTER TABLE amm_trades ALTER COLUMN market_cap TYPE NUMERIC(20,0) USING market_cap::numeric;
                END IF;
            END$$;

            DO $$
            DECLARE
                pk_col_count int;
            BEGIN
                SELECT COUNT(*) INTO pk_col_count
                FROM pg_index i, unnest(i.indkey) AS k
                WHERE i.indrelid = 'amm_trades'::regclass AND i.indisprimary;

                IF pk_col_count = 1 THEN
                    EXECUTE 'ALTER TABLE amm_trades DROP CONSTRAINT amm_trades_pkey';
                    EXECUTE 'ALTER TABLE amm_trades ADD PRIMARY KEY (tx_signature, ix_index)';
                END IF;
            END$$;

            CREATE INDEX IF NOT EXISTS idx_amm_trades_mint ON amm_trades(mint_address);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_pool ON amm_trades(pool_address);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_quote_mint ON amm_trades(quote_mint_address);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_slot ON amm_trades(slot);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_user ON amm_trades(user_pubkey);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_created_at ON amm_trades(created_at);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_success ON amm_trades(success);",
        )
        .await?;

    info!("Ensured amm_trades table and indexes exist");
    Ok(())
}

/// Highest durably committed slot, used as an inclusive reconnect/restart
/// replay checkpoint. Replaying a safety window is harmless because inserts
/// are idempotent upserts.
pub async fn latest_trade_slot(pool: &Pool) -> Result<Option<u64>> {
    let client = pool.get().await?;
    let row = client
        .query_one("SELECT MAX(slot) FROM amm_trades", &[])
        .await?;
    let slot: Option<i64> = row.get(0);
    Ok(slot.and_then(|value| u64::try_from(value).ok()))
}

/// Batch insert trades using a parameterized, idempotent upsert.
///
/// Uses a single client checked out from the pool for the whole batch — callers
/// chunking large flushes should call this once per chunk, not hold their own
/// connection across multiple calls.
pub async fn batch_insert_trades(pool: &Pool, trades: &[TradeRecord]) -> Result<()> {
    if trades.is_empty() {
        return Ok(());
    }

    let client = pool.get().await?;

    const COLS: usize = 22;
    let mut query_parts = Vec::with_capacity(trades.len());
    let token_amounts: Vec<String> = trades
        .iter()
        .map(|trade| trade.token_amount.to_string())
        .collect();
    let quote_amounts: Vec<String> = trades
        .iter()
        .map(|trade| trade.sol_amount.to_string())
        .collect();
    let market_caps: Vec<Option<String>> = trades
        .iter()
        .map(|trade| trade.market_cap.map(|value| value.to_string()))
        .collect();
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        Vec::with_capacity(trades.len() * COLS);

    for (i, trade) in trades.iter().enumerate() {
        let base_idx = i * COLS;
        query_parts.push(format!(
            "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}::text::numeric, ${}::text::numeric, ${}::text::numeric, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
            base_idx + 1,
            base_idx + 2,
            base_idx + 3,
            base_idx + 4,
            base_idx + 5,
            base_idx + 6,
            base_idx + 7,
            base_idx + 8,
            base_idx + 9,
            base_idx + 10,
            base_idx + 11,
            base_idx + 12,
            base_idx + 13,
            base_idx + 14,
            base_idx + 15,
            base_idx + 16,
            base_idx + 17,
            base_idx + 18,
            base_idx + 19,
            base_idx + 20,
            base_idx + 21,
            base_idx + 22,
        ));

        params.push(&trade.tx_signature);
        params.push(&trade.ix_index);
        params.push(&trade.pool_address);
        params.push(&trade.mint_address);
        params.push(&trade.quote_mint_address);
        params.push(&trade.instruction_type);
        params.push(&trade.amount_source);
        params.push(&trade.is_cpi);
        params.push(&trade.user_pubkey);
        params.push(&trade.is_buy);
        params.push(&token_amounts[i]);
        params.push(&quote_amounts[i]);
        params.push(&market_caps[i]);
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

    let query = format!(
        "INSERT INTO amm_trades (tx_signature, ix_index, pool_address, mint_address, quote_mint_address, instruction_type, amount_source, is_cpi, user_pubkey, is_buy, token_amount, sol_amount, market_cap, is_usdc, success, slot, created_at, priority_fee, transfer_tip, tip_provider, compute_units_consumed, lamports_per_compute_unit) VALUES {} \
         ON CONFLICT (tx_signature, ix_index) DO UPDATE SET \
         pool_address = EXCLUDED.pool_address, mint_address = EXCLUDED.mint_address, \
         quote_mint_address = EXCLUDED.quote_mint_address, instruction_type = EXCLUDED.instruction_type, \
         amount_source = EXCLUDED.amount_source, is_cpi = EXCLUDED.is_cpi, user_pubkey = EXCLUDED.user_pubkey, \
         is_buy = EXCLUDED.is_buy, token_amount = EXCLUDED.token_amount, \
         sol_amount = EXCLUDED.sol_amount, market_cap = EXCLUDED.market_cap, \
         is_usdc = EXCLUDED.is_usdc, success = EXCLUDED.success, slot = EXCLUDED.slot, \
         priority_fee = EXCLUDED.priority_fee, transfer_tip = EXCLUDED.transfer_tip, \
         tip_provider = EXCLUDED.tip_provider, compute_units_consumed = EXCLUDED.compute_units_consumed, \
         lamports_per_compute_unit = EXCLUDED.lamports_per_compute_unit",
        query_parts.join(",")
    );

    client.execute(&query, &params).await?;
    Ok(())
}

/// Spawn a background task that receives TradeRecords from a channel and batch-inserts them.
/// Flushes every 60 seconds or when the buffer reaches 500 trades.
pub fn spawn_batch_inserter(
    pool: Arc<Pool>,
    mut receiver: mpsc::Receiver<TradeRecord>,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer: Vec<TradeRecord> = Vec::with_capacity(500);
        let mut flush_ticker = interval(Duration::from_secs(60));
        flush_ticker.tick().await; // skip first immediate tick

        loop {
            tokio::select! {
                _ = flush_ticker.tick() => {
                    if !buffer.is_empty() {
                        flush_buffer(&pool, &mut buffer).await;
                    }
                    if !running.load(Ordering::Relaxed) {
                        break;
                    }
                }
                trade = receiver.recv() => {
                    match trade {
                        Some(record) => {
                            buffer.push(record);
                            if buffer.len() == 500 {
                                flush_buffer(&pool, &mut buffer).await;
                            }
                        }
                        None => {
                            // Channel closed (sender dropped during shutdown)
                            if !buffer.is_empty() {
                                flush_buffer(&pool, &mut buffer).await;
                            }
                            break;
                        }
                    }
                }
            }
        }

        // Drain any remaining items in the channel
        while let Ok(record) = receiver.try_recv() {
            buffer.push(record);
        }
        if !buffer.is_empty() {
            flush_buffer(&pool, &mut buffer).await;
        }

        info!("Batch inserter shut down");
    })
}

/// Delete records outside the intentional rolling 14-day retention window.
pub async fn prune_old_trades(pool: &Pool) -> Result<u64> {
    let client = pool.get().await?;
    let rows = client
        .execute(
            "DELETE FROM amm_trades WHERE created_at < NOW() - INTERVAL '14 days'",
            &[],
        )
        .await?;
    Ok(rows)
}

pub fn spawn_trade_pruner(
    pool: Arc<Pool>,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(3600));
        ticker.tick().await;
        while running.load(Ordering::Relaxed) {
            ticker.tick().await;
            match prune_old_trades(&pool).await {
                Ok(count) if count > 0 => info!("Pruned {} old trades", count),
                Err(e) => warn!("Failed to prune old trades: {:?}", e),
                _ => {}
            }
        }
    })
}

async fn flush_buffer(pool: &Pool, buffer: &mut Vec<TradeRecord>) {
    let pending = std::mem::take(buffer);
    let count = pending.len();
    let mut retry = Vec::new();

    // Chunks of 500 keep us well under the 65,535 bound parameter limit
    // (500 * 22 = 11,000).
    for chunk in pending.chunks(500) {
        if let Err(e) = batch_insert_trades(pool, chunk).await {
            warn!("Failed to batch insert {} trades: {:?}", chunk.len(), e);
            retry.extend_from_slice(chunk);
        }
    }

    if count > 0 {
        info!(
            "Flushed {} trades to database; {} retained for retry",
            count - retry.len(),
            retry.len()
        );
    }
    buffer.extend(retry);
}
