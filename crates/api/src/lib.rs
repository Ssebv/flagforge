//! # flagforge-api
//!
//! The HTTP layer: authentication, the management API, and the SDK-facing
//! evaluation endpoints.
//!
//! Exposed as a library as well as a binary so integration tests can build the
//! real router over a real database and drive it directly — no process to
//! spawn, no port to guess, and no risk of testing something other than what
//! ships.

#![forbid(unsafe_code)]

pub mod auth;
pub mod cache;
pub mod config;
pub mod dashboard;
pub mod error;
pub mod openapi;
pub mod rate_limit;
pub mod routes;
pub mod state;
pub mod telemetry;

pub use config::Config;
pub use error::{ApiError, ApiResult};
pub use routes::router;
pub use state::AppState;
