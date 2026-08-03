//! What using FlagForge from a service actually looks like.
//!
//! ```console
//! $ FLAGFORGE_URL=http://localhost:8080 FLAGFORGE_KEY=ff_srv_… \
//!     cargo run --example usage -p flagforge-sdk
//! ```

use std::time::Duration;

use flagforge_sdk::{Client, EvaluationContext};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The SDK logs refresh failures at warn; without a subscriber they vanish
    // and "my flags went stale" becomes a mystery.
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    let url = std::env::var("FLAGFORGE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let key = std::env::var("FLAGFORGE_KEY").map_err(|_| "set FLAGFORGE_KEY to a server key")?;

    // One client per process, created at start-up and shared. `connect` fails
    // fast on a bad key or URL, which is what you want in a deploy.
    let flags = Client::builder(url, key).poll_interval(Duration::from_secs(15)).connect().await?;

    println!(
        "connected to {} (version {})\n",
        flags.environment().unwrap_or_default(),
        flags.version().unwrap_or_default()
    );

    // A context is whatever your request already knows about the caller.
    let user = EvaluationContext::new("user-42")
        .with("plan", "pro")
        .with("country", "ES")
        .with("seats", 25i64);

    // The common case. No await: the answer is already in memory.
    if flags.is_enabled("checkout.v2", &user, false) {
        println!("checkout.v2  -> new checkout");
    } else {
        println!("checkout.v2  -> old checkout");
    }

    // The full decision, when you want to log *why* alongside the what.
    let decision = flags.evaluate("checkout.v2", &user, false.into());
    println!("             reason: {:?}", decision.reason);

    // Multivariate flags carry configuration, not just on/off.
    println!("banner       -> {}", flags.string_value("loyalty.banner", &user, "control"));

    println!("\nevery flag in this environment:");
    for decision in flags.evaluate_all(&user) {
        println!(
            "  {:<24} {:<10} {:?}",
            decision.flag_key,
            decision.variant.as_deref().unwrap_or("—"),
            decision.reason
        );
    }

    Ok(())
}
