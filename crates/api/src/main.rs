//! FlagForge server entry point.

use std::sync::Arc;

use anyhow::Context;
use flagforge_api::cache::spawn_refresher;
use flagforge_api::config::Config;
use flagforge_api::state::AppState;
use flagforge_api::{routes, telemetry};
use flagforge_storage::PoolConfig;
use tokio::signal;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Only for local development; in a container the environment *is* the
    // environment, and a stray .env would silently shadow it.
    let _ = dotenvy::dotenv();

    let config = Config::from_env().map_err(|errors| {
        let details = errors.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n");
        anyhow::anyhow!("invalid configuration:\n{details}")
    })?;

    telemetry::init_tracing(config.environment);
    let metrics = telemetry::init_metrics().context("failed to install the metrics recorder")?;

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        environment = ?config.environment,
        address = %config.server.address,
        "starting flagforge"
    );

    let mut pool_config = PoolConfig::new(&config.database.url);
    pool_config.max_connections = config.database.max_connections;

    let pool = flagforge_storage::connect(&pool_config)
        .await
        .context("could not connect to the database")?;

    if config.database.auto_migrate {
        flagforge_storage::migrate(&pool).await.context("failed to apply migrations")?;
        tracing::info!("migrations are up to date");
    }

    let database_url = config.database.url.clone();
    let refresh_interval = config.cache.refresh_interval;
    let address = config.server.address;
    let shutdown_grace = config.server.shutdown_grace;
    let show_docs = !config.environment.is_production();

    let state = AppState::new(pool, config);
    let refreshers = spawn_refresher(Arc::clone(&state.cache), database_url, refresh_interval);
    let app = routes::router(state, metrics);

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("could not bind {address}"))?;

    tracing::info!(%address, "listening");
    if show_docs {
        tracing::info!("API documentation at http://{address}/docs");
    }

    // The server drains on notify; the notify fires on a signal. Splitting it
    // this way is what lets the drain itself be time-boxed below.
    let drain = Arc::new(Notify::new());
    let server = {
        let drain = Arc::clone(&drain);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { drain.notified().await })
                .await
        })
    };

    await_signal().await;
    tracing::info!(grace_secs = shutdown_grace.as_secs(), "draining in-flight requests");
    drain.notify_waiters();

    // A request wedged behind a stuck query must not hold the deploy open
    // forever; past the grace period we stop waiting for it.
    match tokio::time::timeout(shutdown_grace, server).await {
        Ok(Ok(result)) => result.context("server error")?,
        Ok(Err(join_error)) => {
            return Err(anyhow::anyhow!(join_error)).context("server task panicked");
        }
        Err(_) => tracing::warn!("grace period elapsed with requests still in flight"),
    }

    for handle in refreshers {
        handle.abort();
    }

    tracing::info!("shutdown complete");
    Ok(())
}

/// Resolves on SIGINT or SIGTERM.
///
/// SIGTERM is the one that matters: it is what an orchestrator sends before
/// replacing a pod. A server that ignores it gets killed mid-request, and an
/// SDK that sees a connection reset falls back to its hard-coded default —
/// which looks exactly like a flag being turned off.
async fn await_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install the Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install the SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT"),
        () = terminate => tracing::info!("received SIGTERM"),
    }
}
