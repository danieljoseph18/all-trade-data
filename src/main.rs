#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod database;
mod grpc;
mod handler;
mod utils;

use anyhow::Result;
use dotenv::dotenv;
use log::{error, info};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<()> {
    // Catch panics and log them
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("[PANIC] {}", panic_info);
        let _ = std::fs::write("/tmp/all-trade-data-panic.txt", format!("{}", panic_info));
    }));

    dotenv().ok();

    utils::setup_logger().expect("Failed to initialize logger");

    // READONLY=true: skip DB writes and log every parsed trade.
    // Used to validate extraction correctness against an independent RPC source.
    let readonly = std::env::var("READONLY")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    info!(
        "Starting All Trade Data collector ({} mode)",
        if readonly {
            "READONLY/validation"
        } else {
            "write"
        }
    );

    // Shared running flag for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));

    // Initialize database pool
    info!("Setting up database connection...");
    let db_pool = match database::init_pool().await {
        Ok(pool) => {
            info!("Database connection established");
            pool
        }
        Err(e) => {
            error!("Failed to connect to database: {}", e);
            panic!("Database connection required");
        }
    };

    // In readonly we skip table creation/migration and whitelist loading.
    if !readonly {
        database::ensure_table(&db_pool).await?;
        database::ensure_alt_table(&db_pool).await?;
    }

    let whitelist = if readonly {
        info!("READONLY: bypassing whitelist; pool-safety filters remain enabled");
        Arc::new(dashmap::DashSet::new())
    } else {
        info!("Loading whitelisted mints...");
        database::load_whitelist(&db_pool).await?
    };

    // stonk.fun post-migration (Raydium CPMM → alt_trades). The alt_mints
    // view is needed even in READONLY: it identifies which leg of a swap is
    // the launch and supplies the quote mint / supply the decode depends on.
    // Write mode tracks whitelisted & not blacklisted rows; READONLY widens
    // to every migrated launch so the whole Stonk flow can be validated.
    let alt_scope = if readonly {
        database::AltMintScope::Migrated
    } else {
        database::AltMintScope::Whitelisted
    };
    info!("Loading alt_mints ({:?})...", alt_scope);
    let alt_mints = database::load_alt_mints(&db_pool, alt_scope).await?;

    // Create the trade record channels
    let (sender, receiver) = tokio::sync::mpsc::channel(10_000);
    let (alt_sender, alt_receiver) = tokio::sync::mpsc::channel(10_000);

    // Background DB tasks only spawn in write mode.
    let (whitelist_handle, alt_mints_handle, inserter_handle, alt_inserter_handle, pruner_handle) =
        if readonly {
            (None, None, None, None, None)
        } else {
            (
                Some(database::spawn_whitelist_refresh(
                    db_pool.clone(),
                    whitelist.clone(),
                    running.clone(),
                )),
                Some(database::spawn_alt_mints_refresh(
                    db_pool.clone(),
                    alt_mints.clone(),
                    alt_scope,
                    running.clone(),
                )),
                Some(database::spawn_batch_inserter(
                    db_pool.clone(),
                    receiver,
                    running.clone(),
                )),
                Some(database::spawn_batch_inserter(
                    db_pool.clone(),
                    alt_receiver,
                    running.clone(),
                )),
                Some(database::spawn_trade_pruner(
                    db_pool.clone(),
                    running.clone(),
                )),
            )
        };

    // One gRPC stream carries PumpSwap and Raydium CPMM txs; each handler
    // self-filters by program id.
    let amm_handler = handler::create_amm_handler(whitelist.clone(), sender.clone(), readonly);
    let cpmm_handler = handler::create_cpmm_handler(alt_mints.clone(), alt_sender.clone(), readonly);
    let tx_handler = move |tx: yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo,
                           slot: u64| {
        let amm = amm_handler(tx.clone(), slot);
        let cpmm = cpmm_handler(tx, slot);
        Box::pin(async move {
            let (a, c) = tokio::join!(amm, cpmm);
            a.and(c)
        })
            as std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
    };

    // Replay inclusively from just before the highest durably committed slot.
    // The request is reused on every reconnect, so anything received but not
    // committed before a disconnect/crash is delivered again and safely upserted.
    // amm_trades is the checkpoint for the shared stream: both inserters flush
    // on the same cadence, and alt_trades (a handful of whitelisted stonk.fun
    // mints) can be silent for long stretches, so its own max slot would
    // force needlessly deep replays.
    // GRPC_FROM_SLOT is an explicit override for deeper historical backfills.
    let replay_from_slot = match std::env::var("GRPC_FROM_SLOT") {
        Ok(value) => Some(
            value
                .parse::<u64>()
                .map_err(|e| anyhow::anyhow!("GRPC_FROM_SLOT must be an unsigned slot: {e}"))?,
        ),
        Err(_) => database::latest_trade_slot(&db_pool)
            .await?
            .map(|slot| slot.saturating_sub(32)),
    };

    info!(
        "Initializing gRPC connection from slot {:?}...",
        replay_from_slot
    );
    let grpc_handle = grpc::init_grpc_connection(tx_handler, replay_from_slot).await?;

    // Listen for shutdown signals
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())?;

    info!("Collector running. Waiting for shutdown signal...");

    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received Ctrl+C (SIGINT). Initiating shutdown...");
        }
        _ = sigterm.recv() => {
            info!("Received SIGTERM. Initiating shutdown...");
        }
    }

    error!("[SHUTDOWN] Initiating graceful shutdown...");

    // Watchdog: force abort if shutdown takes >5s
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        eprintln!("[WATCHDOG] Shutdown took >5s, forcing abort");
        std::process::abort();
    });

    // Step 1: Signal running flag to stop background tasks
    running.store(false, std::sync::atomic::Ordering::Relaxed);

    // Step 2: Abort gRPC stream
    error!("[SHUTDOWN] Aborting gRPC handle...");
    grpc_handle.abort();

    // Step 3: Stop maintenance tasks.
    if let Some(h) = whitelist_handle {
        error!("[SHUTDOWN] Aborting whitelist refresh...");
        h.abort();
    }
    if let Some(h) = alt_mints_handle {
        error!("[SHUTDOWN] Aborting alt_mints refresh...");
        h.abort();
    }
    if let Some(h) = pruner_handle {
        error!("[SHUTDOWN] Aborting trade pruner...");
        h.abort();
    }

    // Step 4: Drop senders to signal the batch inserters to drain and stop
    error!("[SHUTDOWN] Dropping trade senders...");
    drop(sender);
    drop(alt_sender);

    // Step 5: Wait for the batch inserters to finish draining
    let drain = async {
        if let Some(h) = inserter_handle {
            error!("[SHUTDOWN] Waiting for amm_trades batch inserter to finish...");
            let _ = h.await;
        }
        if let Some(h) = alt_inserter_handle {
            error!("[SHUTDOWN] Waiting for alt_trades batch inserter to finish...");
            let _ = h.await;
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), drain).await;

    // Step 6: Close DB pool
    error!("[SHUTDOWN] Closing DB pool...");
    db_pool.close();

    // Step 7: Brief sleep for cleanup
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    error!("[SHUTDOWN] Exiting...");
    std::process::exit(0)
}
