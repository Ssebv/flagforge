//! In-memory environment snapshots.
//!
//! Evaluation is the hot path — an SDK may call it on every request its own
//! service handles — so it must not touch Postgres. This cache holds one
//! immutable snapshot per environment and swaps it wholesale when the database
//! says the configuration changed.
//!
//! Two properties are worth calling out:
//!
//! * **Reads are lock-free.** A slot is an `ArcSwapOption`, so a reader clones
//!   an `Arc` and leaves; it never contends with a concurrent reload.
//! * **Loads are single-flighted.** A cold miss on a busy environment would
//!   otherwise send one identical query per in-flight request. The first
//!   caller loads; the rest wait on the same mutex and find the result already
//!   there.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use flagforge_core::EnvironmentSnapshot;
use flagforge_storage::{ChangeListener, PgPool, StorageError, snapshot};
use tokio::sync::Mutex;
use uuid::Uuid;

/// How long to wait before reconnecting a dropped `LISTEN` connection.
const LISTENER_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct Slot {
    value: ArcSwapOption<EnvironmentSnapshot>,
    /// Held only while loading, never while reading.
    load: Mutex<()>,
}

#[derive(Debug)]
pub struct SnapshotCache {
    pool: PgPool,
    slots: DashMap<Uuid, Arc<Slot>>,
}

impl SnapshotCache {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, slots: DashMap::new() }
    }

    /// Returns the environment's snapshot, loading it on first use.
    pub async fn get(
        &self,
        environment_id: Uuid,
    ) -> Result<Arc<EnvironmentSnapshot>, StorageError> {
        let slot = self.slot(environment_id);

        if let Some(snapshot) = slot.value.load_full() {
            metrics::counter!("flagforge_snapshot_cache_hits_total").increment(1);
            return Ok(snapshot);
        }

        let _guard = slot.load.lock().await;

        // Another task may have loaded it while we waited for the lock.
        if let Some(snapshot) = slot.value.load_full() {
            metrics::counter!("flagforge_snapshot_cache_hits_total").increment(1);
            return Ok(snapshot);
        }

        metrics::counter!("flagforge_snapshot_cache_misses_total").increment(1);
        let loaded = Arc::new(snapshot::load(&self.pool, environment_id).await?);
        slot.value.store(Some(Arc::clone(&loaded)));

        Ok(loaded)
    }

    /// Reloads an environment that is already cached.
    ///
    /// Environments nobody has asked for are skipped — a change to a project
    /// this node never serves should not make it do work.
    pub async fn refresh(&self, environment_id: Uuid) {
        let Some(slot) = self.slots.get(&environment_id).map(|s| Arc::clone(&s)) else {
            return;
        };

        let _guard = slot.load.lock().await;
        match snapshot::load(&self.pool, environment_id).await {
            Ok(fresh) => {
                let version = fresh.version;
                slot.value.store(Some(Arc::new(fresh)));
                tracing::debug!(%environment_id, version, "refreshed environment snapshot");
                metrics::counter!("flagforge_snapshot_refreshes_total").increment(1);
            }
            Err(StorageError::NotFound { .. }) => {
                // The environment was deleted; stop serving it entirely.
                // The local `Arc` keeps the slot alive until the guard drops.
                self.slots.remove(&environment_id);
                tracing::info!(%environment_id, "dropped snapshot for a deleted environment");
            }
            Err(error) => {
                // Keep serving the previous snapshot. Stale flags are a much
                // smaller problem than flags that suddenly stop resolving
                // because the database blinked.
                tracing::warn!(
                    %environment_id,
                    %error,
                    "failed to refresh snapshot, continuing to serve the cached one"
                );
                metrics::counter!("flagforge_snapshot_refresh_failures_total").increment(1);
            }
        }
    }

    /// Reloads everything currently cached; the periodic safety net.
    pub async fn refresh_all(&self) {
        for environment_id in self.cached_environments() {
            self.refresh(environment_id).await;
        }
    }

    pub fn cached_environments(&self) -> Vec<Uuid> {
        self.slots.iter().map(|entry| *entry.key()).collect()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn slot(&self, environment_id: Uuid) -> Arc<Slot> {
        Arc::clone(&self.slots.entry(environment_id).or_default())
    }
}

/// Keeps the cache fresh for the lifetime of the process.
///
/// Runs two independent mechanisms because they fail differently: `LISTEN`
/// gives sub-second invalidation but dies silently if the connection drops,
/// while the periodic sweep is slow but cannot get stuck.
pub fn spawn_refresher(
    cache: Arc<SnapshotCache>,
    database_url: String,
    interval: Duration,
) -> Vec<tokio::task::JoinHandle<()>> {
    let listener_cache = Arc::clone(&cache);
    let listener = tokio::spawn(async move {
        loop {
            match ChangeListener::connect(&database_url).await {
                Ok(mut listener) => {
                    tracing::info!("listening for configuration changes");
                    loop {
                        match listener.next_changed().await {
                            Ok(environment_id) => listener_cache.refresh(environment_id).await,
                            Err(error) => {
                                tracing::warn!(%error, "change listener disconnected");
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "could not open a change listener");
                }
            }
            tokio::time::sleep(LISTENER_RETRY_DELAY).await;
        }
    });

    let sweeper = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Without this, a stalled sweep would be followed by a burst of
        // catch-up ticks all firing at once.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // the first tick completes immediately

        loop {
            ticker.tick().await;
            cache.refresh_all().await;
        }
    });

    vec![listener, sweeper]
}
