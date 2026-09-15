//! Read-only view of `alt_mints` (written by strategy-data) that tells the
//! CPMM handler which stonk.fun mints to record and how to interpret their
//! swaps: the launch quote mint (which pins the migration pool PDA), the
//! create-time supply (market cap) and the quote decimals.

use anyhow::Result;
use dashmap::DashMap;
use deadpool_postgres::Pool;
use log::{error, info};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, interval};

/// Per-mint facts needed to decode a post-migration swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltMintInfo {
    /// LaunchLab pool quote mint; immutable after launch. Together with the
    /// Stonk `amm_config` it determines the migration pool PDA.
    pub quote_mint: String,
    /// Create-time supply in base atomic units (10^15 so far).
    pub total_supply: u64,
    pub base_decimals: i16,
    /// NULL in `alt_mints` until strategy-data has seen a quote token
    /// balance; the handler falls back to the tx's token-balance rows.
    pub quote_decimals: Option<i16>,
    /// CPMM pool recorded from an observed `migrate_to_cpswap` (or adopted
    /// from a swap). Accepted alongside the derived PDA.
    pub cpmm_pool: Option<String>,
}

pub type AltMintMap = Arc<DashMap<String, AltMintInfo>>;

/// Which `alt_mints` rows to track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltMintScope {
    /// `whitelisted = true AND blacklisted = false` — write mode.
    Whitelisted,
    /// Every migrated launch regardless of whitelist — READONLY validation,
    /// so the operator sees all post-migration Stonk flow, not just the
    /// handful of whitelisted mints.
    Migrated,
}

impl AltMintScope {
    fn predicate(self) -> &'static str {
        match self {
            AltMintScope::Whitelisted => "whitelisted = true AND blacklisted = false",
            AltMintScope::Migrated => "migrated = true",
        }
    }
}

async fn fetch(pool: &Pool, scope: AltMintScope) -> Result<Vec<(String, AltMintInfo)>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            &format!(
                "SELECT token_address, quote_mint, total_supply, base_decimals, quote_decimals, cpmm_pool \
                 FROM alt_mints WHERE {}",
                scope.predicate()
            ),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let total_supply: i64 = row.get(2);
            (
                row.get(0),
                AltMintInfo {
                    quote_mint: row.get(1),
                    total_supply: u64::try_from(total_supply).unwrap_or(0),
                    base_decimals: row.get(3),
                    quote_decimals: row.get(4),
                    cpmm_pool: row.get(5),
                },
            )
        })
        .collect())
}

pub async fn load_alt_mints(pool: &Pool, scope: AltMintScope) -> Result<AltMintMap> {
    let map = Arc::new(DashMap::new());
    for (mint, info) in fetch(pool, scope).await? {
        map.insert(mint, info);
    }
    info!(
        "Loaded {} alt_mints ({:?}) for stonk.fun CPMM tracking",
        map.len(),
        scope
    );
    Ok(map)
}

/// Replace the map contents atomically per key: rows that left the scope are
/// removed, changed rows (e.g. `cpmm_pool` learned after migration) updated.
pub async fn refresh_alt_mints(
    pool: &Pool,
    map: &DashMap<String, AltMintInfo>,
    scope: AltMintScope,
) -> Result<()> {
    let fresh = fetch(pool, scope).await?;
    let keep: std::collections::HashSet<&str> = fresh.iter().map(|(m, _)| m.as_str()).collect();
    map.retain(|mint, _| keep.contains(mint.as_str()));
    for (mint, info) in fresh {
        map.insert(mint, info);
    }
    Ok(())
}

pub fn spawn_alt_mints_refresh(
    pool: Arc<Pool>,
    map: AltMintMap,
    scope: AltMintScope,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(300));
        ticker.tick().await;
        while running.load(Ordering::Relaxed) {
            ticker.tick().await;
            match refresh_alt_mints(&pool, &map, scope).await {
                Ok(()) => info!("alt_mints refreshed: {} mints", map.len()),
                Err(e) => error!("Failed to refresh alt_mints: {:?}", e),
            }
        }
    })
}
