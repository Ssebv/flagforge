//! Shared application state.

use std::sync::Arc;

use flagforge_storage::PgPool;

use crate::auth::jwt::TokenIssuer;
use crate::cache::SnapshotCache;
use crate::config::Config;

/// Handed to every handler by axum. Cheap to clone: everything inside is
/// either an `Arc` or already reference-counted internally.
#[derive(Clone, Debug)]
pub struct AppState {
    pub pool: PgPool,
    pub cache: Arc<SnapshotCache>,
    pub tokens: TokenIssuer,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Self {
        let tokens = TokenIssuer::new(&config.auth.jwt_secret, config.auth.token_ttl);
        Self {
            cache: Arc::new(SnapshotCache::new(pool.clone())),
            pool,
            tokens,
            config: Arc::new(config),
        }
    }
}
