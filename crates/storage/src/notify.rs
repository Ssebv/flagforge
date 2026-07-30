//! Listening for configuration changes.
//!
//! The database is the single source of truth about *when* a snapshot went
//! stale. Triggers emit `NOTIFY` on every write (see the migrations) and each
//! API node holds one `LISTEN` connection, so a change made on any node — or
//! by a human in psql — invalidates every node's cache within milliseconds,
//! without any node-to-node messaging.

use sqlx::postgres::PgListener;
use uuid::Uuid;

/// Channel the triggers publish to.
pub const CHANGE_CHANNEL: &str = "flagforge_env_changed";

/// A long-lived subscription to configuration changes.
#[derive(Debug)]
pub struct ChangeListener {
    inner: PgListener,
}

impl ChangeListener {
    /// Opens a dedicated connection and subscribes.
    ///
    /// Deliberately not taken from the main pool: a `LISTEN` connection is
    /// occupied forever, and starving the request pool to watch for changes
    /// would trade the problem for a worse one.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let mut listener = PgListener::connect(database_url).await?;
        listener.listen(CHANGE_CHANNEL).await?;
        Ok(Self { inner: listener })
    }

    /// Waits for the next changed environment.
    ///
    /// Payloads that are not UUIDs are skipped rather than returned as errors:
    /// an unrelated `NOTIFY` on the same channel should not tear down the
    /// listener.
    pub async fn next_changed(&mut self) -> Result<Uuid, sqlx::Error> {
        loop {
            let notification = self.inner.recv().await?;
            match notification.payload().parse::<Uuid>() {
                Ok(id) => return Ok(id),
                Err(_) => {
                    tracing::warn!(
                        channel = CHANGE_CHANNEL,
                        payload = notification.payload(),
                        "ignoring change notification with an unparseable payload"
                    );
                }
            }
        }
    }
}
