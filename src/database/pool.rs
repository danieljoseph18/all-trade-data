use anyhow::Result;
use deadpool_postgres::{Config, Pool, Runtime};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio_postgres::{NoTls, config::Config as TokioConfig};

static DB_POOL: OnceCell<Arc<Pool>> = OnceCell::const_new();

/// Initialize the shared database pool
pub async fn init_pool() -> Result<Arc<Pool>> {
    DB_POOL
        .get_or_init(|| async {
            let database_url = std::env::var("PG_DATABASE_URL")
                .expect("PG_DATABASE_URL environment variable must be set");

            let tokio_config: TokioConfig =
                database_url.parse().expect("Failed to parse database URL");

            let mut cfg = Config::new();
            cfg.user = tokio_config.get_user().map(|s| s.to_string());
            cfg.password = tokio_config
                .get_password()
                .map(|s| String::from_utf8_lossy(s).to_string());
            cfg.dbname = tokio_config.get_dbname().map(|s| s.to_string());

            if let Some(host) = tokio_config.get_hosts().first() {
                match host {
                    tokio_postgres::config::Host::Tcp(hostname) => {
                        cfg.host = Some(hostname.clone());
                    }
                    _ => {}
                }
            }

            if let Some(port) = tokio_config.get_ports().first() {
                cfg.port = Some(*port);
            }

            cfg.pool = Some(deadpool_postgres::PoolConfig {
                max_size: 3,
                timeouts: deadpool_postgres::Timeouts {
                    wait: Some(std::time::Duration::from_secs(30)),
                    create: Some(std::time::Duration::from_secs(5)),
                    recycle: Some(std::time::Duration::from_secs(5)),
                },
                ..Default::default()
            });

            let pool = cfg
                .create_pool(Some(Runtime::Tokio1), NoTls)
                .expect("Failed to create database pool");

            Arc::new(pool)
        })
        .await;

    Ok(get_pool().await)
}

/// Get the shared database pool
pub async fn get_pool() -> Arc<Pool> {
    DB_POOL
        .get()
        .expect("Database pool not initialized. Call init_pool() first.")
        .clone()
}
