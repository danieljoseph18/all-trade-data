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

    // Kill any existing instance to prevent DB pool contention
    let pid_file = "/tmp/all-trade-data.pid";
    let current_pid = std::process::id();
    if let Ok(old_pid_str) = std::fs::read_to_string(pid_file) {
        if let Ok(old_pid) = old_pid_str.trim().parse::<u32>() {
            if old_pid != current_pid {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &old_pid.to_string()])
                    .output();
            }
        }
    }
    if let Ok(output) = std::process::Command::new("pgrep")
        .arg("all-trade-data")
        .output()
    {
        if output.status.success() {
            for pid_str in String::from_utf8_lossy(&output.stdout).lines() {
                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                    if pid != current_pid {
                        let _ = std::process::Command::new("kill")
                            .args(["-9", &pid.to_string()])
                            .output();
                    }
                }
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = std::fs::write(pid_file, current_pid.to_string());

    utils::setup_logger().expect("Failed to initialize logger");
    info!("Starting All Trade Data collector");

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

    // Ensure the amm_trades table exists
    database::ensure_table(&db_pool).await?;

    // Load whitelisted mints
    info!("Loading whitelisted mints...");
    let whitelist = database::load_whitelist(&db_pool).await?;

    // Create the trade record channel
    let (sender, receiver) = tokio::sync::mpsc::channel(10_000);

    // Spawn the whitelist refresh task
    let whitelist_handle =
        database::spawn_whitelist_refresh(db_pool.clone(), whitelist.clone(), running.clone());

    // Spawn the batch inserter task
    let inserter_handle =
        database::spawn_batch_inserter(db_pool.clone(), receiver, running.clone());

    // Spawn the trade pruner task (deletes trades older than 14 days, runs hourly)
    let pruner_handle =
        database::spawn_trade_pruner(db_pool.clone(), running.clone());

    // Create the AMM handler
    let amm_handler = handler::create_amm_handler(whitelist.clone(), sender.clone());

    // Grace period for old connections to clean up
    info!("Waiting 3s for old connections to clean up...");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Initialize gRPC connection
    info!("Initializing gRPC connection...");
    let grpc_handle = grpc::init_grpc_connection(amm_handler).await?;

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

    // Step 3: Abort whitelist refresh and trade pruner
    error!("[SHUTDOWN] Aborting whitelist refresh...");
    whitelist_handle.abort();
    error!("[SHUTDOWN] Aborting trade pruner...");
    pruner_handle.abort();

    // Step 4: Drop sender to signal batch inserter to drain and stop
    error!("[SHUTDOWN] Dropping trade sender...");
    drop(sender);

    // Step 5: Wait for batch inserter to finish draining
    error!("[SHUTDOWN] Waiting for batch inserter to finish...");
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        inserter_handle,
    )
    .await;

    // Step 6: Close DB pool
    error!("[SHUTDOWN] Closing DB pool...");
    db_pool.close();

    // Step 7: Brief sleep for cleanup
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    error!("[SHUTDOWN] Exiting...");
    std::process::exit(0)
}
