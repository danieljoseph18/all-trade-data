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
    pub mint_address: String,
    pub user_pubkey: String,
    pub is_buy: bool,
    pub token_amount: i64,
    pub sol_amount: i64,
    pub market_cap: Option<i64>,
    pub slot: i64,
    pub created_at: DateTime<Utc>,
    pub priority_fee: Option<i64>,
    pub transfer_tip: Option<i64>,
    pub tip_provider: Option<String>,
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
                mint_address TEXT NOT NULL,
                user_pubkey TEXT NOT NULL,
                is_buy BOOLEAN NOT NULL,
                token_amount BIGINT NOT NULL,
                sol_amount BIGINT NOT NULL,
                market_cap BIGINT,
                slot BIGINT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                priority_fee BIGINT,
                transfer_tip BIGINT,
                tip_provider TEXT,
                PRIMARY KEY (tx_signature, ix_index)
            );

            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS ix_index INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS priority_fee BIGINT;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS transfer_tip BIGINT;
            ALTER TABLE amm_trades ADD COLUMN IF NOT EXISTS tip_provider TEXT;

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
            CREATE INDEX IF NOT EXISTS idx_amm_trades_slot ON amm_trades(slot);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_user ON amm_trades(user_pubkey);
            CREATE INDEX IF NOT EXISTS idx_amm_trades_created_at ON amm_trades(created_at);",
        )
        .await?;

    info!("Ensured amm_trades table and indexes exist");
    Ok(())
}

/// Batch insert trades using a parameterized query with ON CONFLICT DO NOTHING.
///
/// Uses a single client checked out from the pool for the whole batch — callers
/// chunking large flushes should call this once per chunk, not hold their own
/// connection across multiple calls.
pub async fn batch_insert_trades(pool: &Pool, trades: &[TradeRecord]) -> Result<()> {
    if trades.is_empty() {
        return Ok(());
    }

    let client = pool.get().await?;

    const COLS: usize = 13;
    let mut query_parts = Vec::with_capacity(trades.len());
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        Vec::with_capacity(trades.len() * COLS);

    for (i, trade) in trades.iter().enumerate() {
        let base_idx = i * COLS;
        query_parts.push(format!(
            "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
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
        ));

        params.push(&trade.tx_signature);
        params.push(&trade.ix_index);
        params.push(&trade.mint_address);
        params.push(&trade.user_pubkey);
        params.push(&trade.is_buy);
        params.push(&trade.token_amount);
        params.push(&trade.sol_amount);
        params.push(&trade.market_cap);
        params.push(&trade.slot);
        params.push(&trade.created_at);
        params.push(&trade.priority_fee);
        params.push(&trade.transfer_tip);
        params.push(&trade.tip_provider);
    }

    let query = format!(
        "INSERT INTO amm_trades (tx_signature, ix_index, mint_address, user_pubkey, is_buy, token_amount, sol_amount, market_cap, slot, created_at, priority_fee, transfer_tip, tip_provider) VALUES {} ON CONFLICT (tx_signature, ix_index) DO NOTHING",
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
                            if buffer.len() >= 500 {
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

/// Delete trades older than 14 days.
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

/// Spawn a background task that prunes old trades every hour.
pub fn spawn_trade_pruner(
    pool: Arc<Pool>,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(3600));
        ticker.tick().await; // skip first immediate tick
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
    let count = buffer.len();

    // Chunks of 500 keep us well under the 65,535 bound parameter limit
    // (500 * 13 = 6,500).
    for chunk in buffer.chunks(500) {
        if let Err(e) = batch_insert_trades(pool, chunk).await {
            warn!("Failed to batch insert {} trades: {:?}", chunk.len(), e);
        }
    }

    if count > 0 {
        info!("Flushed {} trades to database", count);
    }

    buffer.clear();
}
