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
    /// How long [`connect`] keeps retrying before giving up.
    ///
    /// Deliberately *not* `acquire_timeout`. That one is short because a
    /// request waiting on an exhausted pool should fail fast; this one is long
    /// because a process starting up should wait for a database that is still
    /// coming up. The same number cannot serve both: a five-second limit on
    /// boot turns an ordinary cold start — a suspended database, a failover, a
    /// restarting sidecar — into a failed deploy.
    pub startup_timeout: Duration,
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
            startup_timeout: Duration::from_secs(30),
        }
    }
}

/// Opens the pool, retrying until [`PoolConfig::startup_timeout`] elapses.
///
/// A database that is not there *yet* is an ordinary condition at boot, not an
/// error: it may be suspended, failing over, or simply slower to start than we
/// are. Retrying costs nothing when it is already up — the first attempt
/// succeeds — and is the difference between a deploy that waits and a deploy
/// that dies.
pub async fn connect(config: &PoolConfig) -> Result<PgPool, sqlx::Error> {
    const RETRY_DELAY: Duration = Duration::from_millis(500);

    let deadline = std::time::Instant::now() + config.startup_timeout;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let result = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(config.acquire_timeout)
            .connect(&config.url)
            .await;

        match result {
            Ok(pool) => {
                if attempt > 1 {
                    tracing::info!(attempt, "database reached after waiting for it to come up");
                }
                return Ok(pool);
            }
            // Out of patience: report the last failure, which is the one that
            // says why.
            Err(error) if std::time::Instant::now() + RETRY_DELAY >= deadline => {
                tracing::error!(attempt, %error, "giving up on the database");
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(attempt, %error, "database not ready yet, retrying");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_budget_is_far_longer_than_the_request_one() {
        let config = PoolConfig::new("postgres://x/y");
        assert!(
            config.startup_timeout > config.acquire_timeout * 4,
            "waiting for a database to boot and waiting for a busy pool are not the same wait"
        );
    }

    /// The retry has to *end*. A database that is genuinely gone must fail the
    /// process rather than hang the deploy forever.
    #[tokio::test]
    async fn connecting_gives_up_once_the_startup_budget_is_spent() {
        let config = PoolConfig {
            startup_timeout: Duration::from_millis(1200),
            acquire_timeout: Duration::from_millis(200),
            // Port 1 is reserved and nothing listens there.
            ..PoolConfig::new("postgres://nobody@127.0.0.1:1/nothing")
        };

        let started = std::time::Instant::now();
        assert!(connect(&config).await.is_err());
        let elapsed = started.elapsed();

        assert!(elapsed >= Duration::from_millis(500), "it did not retry at all: {elapsed:?}");
        assert!(elapsed < Duration::from_secs(10), "it ignored its own deadline: {elapsed:?}");
    }
}
