//! # flagforge-sdk
//!
//! The client a Rust service uses to ask FlagForge questions.
//!
//! It fetches the environment's configuration once, keeps it in memory, and
//! answers every subsequent call locally by running [`flagforge_core`] — the
//! same engine the server runs. So a flag check costs a hash and a few
//! comparisons rather than a network round trip, and it cannot disagree with
//! the server, because it is not a reimplementation.
//!
//! ```no_run
//! use flagforge_sdk::{Client, EvaluationContext};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let flags = Client::builder("https://flags.example.com", "ff_srv_…").connect().await?;
//!
//! let user = EvaluationContext::new("user-42").with("plan", "pro");
//! if flags.is_enabled("checkout.v2", &user, false) {
//!     // new checkout
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## What it does when things go wrong
//!
//! A flag client sits in the request path of the service that embeds it, so
//! its failure modes matter more than its features:
//!
//! * **The server is unreachable at start-up.** [`ClientBuilder::connect`]
//!   returns the error so you can decide; [`ClientBuilder::connect_lazy`]
//!   starts anyway and serves your fallbacks until a refresh succeeds.
//! * **A refresh fails.** The previous configuration keeps being served and a
//!   warning is logged. Stale flags are a far smaller problem than a service
//!   that stops answering because the flag system blinked.
//! * **A flag does not exist.** You get the fallback you passed at the call
//!   site — never a panic, never an error to handle inline.

#![forbid(unsafe_code)]

mod client;
mod error;

pub use client::{Client, ClientBuilder, Snapshot};
pub use error::Error;

// Re-exported so callers do not need to depend on `flagforge-core` directly to
// name the types they must pass in and match on.
pub use flagforge_core::{AttributeValue, Evaluation, EvaluationContext, Reason, VariantValue};
