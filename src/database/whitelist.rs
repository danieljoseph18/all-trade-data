use anyhow::Result;
use dashmap::DashSet;
use deadpool_postgres::Pool;
use log::{error, info};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, interval};

/// Load all whitelisted token addresses from the all_mints table into a DashSet.
pub async fn load_whitelist(pool: &Pool) -> Result<Arc<DashSet<String>>> {
    let whitelist = Arc::new(DashSet::new());
    let client = pool.get().await?;

    let rows = client
        .query(
            "SELECT token_address FROM all_mints WHERE whitelisted = true",
            &[],
        )
        .await?;

    for row in &rows {
        let token_address: String = row.get(0);
        whitelist.insert(token_address);
    }

    info!(
        "Loaded {} whitelisted mints from database",
        whitelist.len()
    );

    Ok(whitelist)
}

/// Refresh the whitelist by clearing and reloading from the database.
pub async fn refresh_whitelist(pool: &Pool, whitelist: &DashSet<String>) -> Result<()> {
    let client = pool.get().await?;

    let rows = client
        .query(
            "SELECT token_address FROM all_mints WHERE whitelisted = true",
            &[],
        )
        .await?;

    whitelist.clear();
    for row in &rows {
        let token_address: String = row.get(0);
        whitelist.insert(token_address);
    }

    Ok(())
}

/// Spawn a background task that refreshes the whitelist every 60 seconds.
pub fn spawn_whitelist_refresh(
    pool: Arc<Pool>,
    whitelist: Arc<DashSet<String>>,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(60));
        ticker.tick().await; // skip first immediate tick

        while running.load(Ordering::Relaxed) {
            ticker.tick().await;

            match refresh_whitelist(&pool, &whitelist).await {
                Ok(()) => {
                    info!("Whitelist refreshed: {} mints", whitelist.len());
                }
                Err(e) => {
                    error!("Failed to refresh whitelist: {:?}", e);
                }
            }
        }
    })
}
