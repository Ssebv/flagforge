//! Connection pool setup and migrations.

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

/// Embedded migrations. Compiled into the binary so a container can bring its
/// own schema up without shipping the `.sql` files alongside it.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
}

impl PoolConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_connections: 20,
            min_connections: 1,
            // Short on purpose: if the pool is exhausted we want a fast 503
            // rather than a pile of requests waiting behind a stuck query.
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

pub async fn connect(config: &PoolConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(config.acquire_timeout)
        .connect(&config.url)
        .await
}

/// Round-trips a trivial query.
///
/// Proves the pool can hand out a *live* connection, which is what readiness
/// actually depends on — a pool with a full but dead connection set looks fine
/// until the first real query.
pub async fn ping(pool: &PgPool) -> bool {
    sqlx::query("SELECT 1").execute(pool).await.is_ok()
}

/// Applies any pending migrations. Safe to call from every replica on boot:
/// sqlx takes an advisory lock, so concurrent starts serialize instead of
/// racing.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}
