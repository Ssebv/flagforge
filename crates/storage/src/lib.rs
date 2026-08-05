//! # flagforge-storage
//!
//! PostgreSQL persistence for FlagForge.
//!
//! Every query is checked against the real schema at compile time by `sqlx`,
//! so a column rename breaks the build rather than production. The API crate
//! depends on this one; this one knows nothing about HTTP.
//!
//! Two design rules run through the whole module:
//!
//! * **Tenancy is a query argument, never a trust assumption.** Lookups take
//!   the caller's `organization_id` and filter on it, so an attacker holding a
//!   valid token for tenant A cannot reach tenant B's rows by guessing UUIDs.
//! * **Races are resolved by the database.** Uniqueness, version bumping and
//!   optimistic concurrency are enforced by constraints and triggers rather
//!   than by read-then-write logic that two replicas could interleave.

#![forbid(unsafe_code)]

pub mod accounts;
pub mod api_keys;
pub mod audit;
pub mod error;
pub mod flags;
pub mod models;
pub mod notify;
pub mod pool;
pub mod projects;
pub mod segments;
pub mod snapshot;

pub use error::{Result, StorageError};
pub use notify::{CHANGE_CHANNEL, ChangeListener};
pub use pool::{MIGRATOR, PoolConfig, connect, migrate};

/// Re-exported so downstream crates do not need their own `sqlx` dependency
/// just to name a pool.
pub use sqlx::PgPool;
