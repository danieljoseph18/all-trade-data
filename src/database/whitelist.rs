use anyhow::Result;
use dashmap::DashSet;
use deadpool_postgres::Pool;
use log::{error, info};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, interval};

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
        whitelist.insert(row.get(0));
    }
    info!("Loaded {} whitelisted mints from database", whitelist.len());
    Ok(whitelist)
}

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
        whitelist.insert(row.get(0));
    }
    Ok(())
}

pub fn spawn_whitelist_refresh(
    pool: Arc<Pool>,
    whitelist: Arc<DashSet<String>>,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(300));
        ticker.tick().await;
        while running.load(Ordering::Relaxed) {
            ticker.tick().await;
            match refresh_whitelist(&pool, &whitelist).await {
                Ok(()) => info!("Whitelist refreshed: {} mints", whitelist.len()),
                Err(e) => error!("Failed to refresh whitelist: {:?}", e),
            }
        }
    })
}
