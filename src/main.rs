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

    // READONLY=true: skip DB writes, bypass whitelist, log every parsed trade.
    // Used to validate extraction correctness against an independent RPC source.
    let readonly = std::env::var("READONLY")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    info!(
        "Starting All Trade Data collector ({} mode)",
        if readonly { "READONLY/validation" } else { "write" }
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

    // In readonly we skip table creation/migration (DDL is a write) and the
    // whitelist load (operator wants to see all trades).
    if !readonly {
        database::ensure_table(&db_pool).await?;
    }

    let whitelist = if readonly {
        info!("READONLY: bypassing whitelist filter — all AMM trades will be logged");
        Arc::new(dashmap::DashSet::new())
    } else {
        info!("Loading whitelisted mints...");
        database::load_whitelist(&db_pool).await?
    };

    // Create the trade record channel
    let (sender, receiver) = tokio::sync::mpsc::channel(10_000);

    // Background DB tasks only spawn in write mode.
    let (whitelist_handle, inserter_handle, pruner_handle) = if readonly {
        (None, None, None)
    } else {
        (
            Some(database::spawn_whitelist_refresh(
                db_pool.clone(),
                whitelist.clone(),
                running.clone(),
            )),
            Some(database::spawn_batch_inserter(
                db_pool.clone(),
                receiver,
                running.clone(),
            )),
            Some(database::spawn_trade_pruner(db_pool.clone(), running.clone())),
        )
    };

    // Create the AMM handler
    let amm_handler = handler::create_amm_handler(whitelist.clone(), sender.clone(), readonly);

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
    if let Some(h) = whitelist_handle {
        error!("[SHUTDOWN] Aborting whitelist refresh...");
        h.abort();
    }
    if let Some(h) = pruner_handle {
        error!("[SHUTDOWN] Aborting trade pruner...");
        h.abort();
    }

    // Step 4: Drop sender to signal batch inserter to drain and stop
    error!("[SHUTDOWN] Dropping trade sender...");
    drop(sender);

    // Step 5: Wait for batch inserter to finish draining
    if let Some(h) = inserter_handle {
        error!("[SHUTDOWN] Waiting for batch inserter to finish...");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), h).await;
    }

    // Step 6: Close DB pool
    error!("[SHUTDOWN] Closing DB pool...");
    db_pool.close();

    // Step 7: Brief sleep for cleanup
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    error!("[SHUTDOWN] Exiting...");
    std::process::exit(0)
}
