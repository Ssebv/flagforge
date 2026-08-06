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

/// What the binary was asked to do.
///
/// Hand-rolled rather than a CLI framework: there is exactly one subcommand,
/// and `clap` would be a dependency and a compile-time cost for parsing three
/// arguments.
enum Command {
    Serve,
    Seed(flagforge_api::seed::Credentials),
}

fn parse_command() -> anyhow::Result<Command> {
    let mut args = std::env::args().skip(1);

    match args.next().as_deref() {
        None => Ok(Command::Serve),
        Some("serve") => Ok(Command::Serve),
        Some("seed") => {
            let mut credentials = flagforge_api::seed::Credentials::default();
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--email" => {
                        credentials.email = args.next().context("--email needs a value")?;
                    }
                    "--password" => {
                        credentials.password =
                            Some(args.next().context("--password needs a value")?);
                    }
                    "--if-empty" => credentials.if_empty = true,
                    other => anyhow::bail!("unknown option `{other}` for `seed`"),
                }
            }
            Ok(Command::Seed(credentials))
        }
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Some(other) => anyhow::bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

const USAGE: &str = "\
flagforge — a multi-tenant feature-flag service

USAGE:
    flagforge [serve]                 Run the API and dashboard (default)
    flagforge seed [OPTIONS]          Fill an empty database with a demo organization

SEED OPTIONS:
    --email <EMAIL>                   Owner address    [default: ada@acme.test]
    --password <PASSWORD>             Owner password. Defaults to a documented
                                      one in development and a generated one in
                                      production, where publishing it would hand
                                      over the deployment
    --if-empty                        Succeed quietly if the database already has
                                      that owner, so this can run on every deploy

Configuration is read from the environment; see .env.example.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Only for local development; in a container the environment *is* the
    // environment, and a stray .env would silently shadow it.
    let _ = dotenvy::dotenv();

    let command = parse_command()?;

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
    pool_config.startup_timeout = config.database.startup_timeout;

    let pool = flagforge_storage::connect(&pool_config)
        .await
        .context("could not connect to the database")?;

    if config.database.auto_migrate {
        flagforge_storage::migrate(&pool).await.context("failed to apply migrations")?;
        tracing::info!("migrations are up to date");
    }

    if let Command::Seed(credentials) = command {
        return flagforge_api::seed::run(&pool, credentials, config.environment).await;
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
    if flagforge_api::dashboard::is_bundled() {
        tracing::info!("dashboard at http://{address}/");
    } else {
        tracing::warn!(
            "no dashboard bundled; run `trunk build --release` in crates/web to include it"
        );
    }
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
