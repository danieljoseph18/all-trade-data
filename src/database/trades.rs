use anyhow::Result;
use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use log::{info, warn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

/// A single AMM trade record to be inserted into the database.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub tx_signature: String,
    pub mint_address: String,
    pub user_pubkey: String,
    pub is_buy: bool,
    pub token_amount: i64,
    pub sol_amount: i64,
    pub market_cap: Option<i64>,
    pub slot: i64,
    pub created_at: DateTime<Utc>,
}

/// Create the amm_trades table and indexes if they don't exist.
pub async fn ensure_table(pool: &Pool) -> Result<()> {
    let client = pool.get().await?;

    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS amm_trades (
                tx_signature TEXT PRIMARY KEY,
                mint_address TEXT NOT NULL,
                user_pubkey TEXT NOT NULL,
                is_buy BOOLEAN NOT NULL,
                token_amount BIGINT NOT NULL,
                sol_amount BIGINT NOT NULL,
                market_cap BIGINT,
                slot BIGINT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL
            );
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
pub async fn batch_insert_trades(pool: &Pool, trades: &[TradeRecord]) -> Result<()> {
    if trades.is_empty() {
        return Ok(());
    }

    let client = pool.get().await?;

    let mut query_parts = Vec::new();
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();

    for (i, trade) in trades.iter().enumerate() {
        let base_idx = i * 9;
        query_parts.push(format!(
            "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
            base_idx + 1,
            base_idx + 2,
            base_idx + 3,
            base_idx + 4,
            base_idx + 5,
            base_idx + 6,
            base_idx + 7,
            base_idx + 8,
            base_idx + 9,
        ));

        params.push(&trade.tx_signature);
        params.push(&trade.mint_address);
        params.push(&trade.user_pubkey);
        params.push(&trade.is_buy);
        params.push(&trade.token_amount);
        params.push(&trade.sol_amount);
        params.push(&trade.market_cap);
        params.push(&trade.slot);
        params.push(&trade.created_at);
    }

    let query = format!(
        "INSERT INTO amm_trades (tx_signature, mint_address, user_pubkey, is_buy, token_amount, sol_amount, market_cap, slot, created_at) VALUES {} ON CONFLICT (tx_signature) DO NOTHING",
        query_parts.join(",")
    );

    client.execute(&query, &params).await?;
    Ok(())
}

/// Spawn a background task that receives TradeRecords from a channel and batch-inserts them.
/// Flushes every 2 seconds or when the buffer reaches 50 trades.
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

    // Chunk into groups of 50 to stay well within Postgres parameter limits
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
