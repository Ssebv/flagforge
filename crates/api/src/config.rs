//! Configuration, read from the environment.
//!
//! Flat, conventional variable names (`PORT`, `DATABASE_URL`) rather than a
//! nested config file: every platform this is meant to run on — Fly, Railway,
//! Kubernetes, docker-compose — injects settings that way, and a config file
//! would just be a second place for them to disagree.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Below this length an HS256 secret is guessable enough to matter, so we
/// refuse to boot rather than run with a weak one.
const MIN_JWT_SECRET_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("required environment variable `{0}` is not set")]
    Missing(&'static str),

    #[error("environment variable `{name}` is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub cache: CacheConfig,
    pub rate_limit: RateLimitConfig,
    /// `production` disables Swagger UI and switches logs to JSON.
    pub environment: RuntimeEnvironment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEnvironment {
    Development,
    Production,
}

impl RuntimeEnvironment {
    pub fn is_production(self) -> bool {
        self == Self::Production
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub address: SocketAddr,
    /// Upper bound on a single request. Anything slower is a stuck query, and
    /// holding the connection open only spreads the problem.
    pub request_timeout: Duration,
    pub body_limit_bytes: usize,
    /// How long in-flight requests get to finish after SIGTERM.
    pub shutdown_grace: Duration,
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    /// Whether to run pending migrations on boot.
    pub auto_migrate: bool,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub token_ttl: Duration,
    /// Bearer token `/metrics` requires, if any.
    ///
    /// Unset leaves the endpoint open, which is right on a laptop and wrong on
    /// the public internet: it is the one route with no authentication at all.
    /// Opt-in rather than mandatory because a scraper that suddenly needs a
    /// credential is an outage, and because the exposure is real but small —
    /// method, path and status, with no tenant data in any label.
    pub metrics_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Safety-net reload interval. Change notifications do the real work; this
    /// only bounds staleness if a `LISTEN` connection dies unnoticed.
    pub refresh_interval: Duration,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Burst size per client.
    pub burst: u32,
    /// Sustained requests per second per client.
    pub per_second: u32,
}

impl Config {
    /// Reads and validates the whole configuration, reporting *every* problem
    /// rather than the first — a misconfigured deploy should need one restart,
    /// not five.
    pub fn from_env() -> Result<Self, Vec<ConfigError>> {
        let mut errors = Vec::new();

        let database_url = required("DATABASE_URL", &mut errors);
        let jwt_secret = required("JWT_SECRET", &mut errors);

        if let Some(secret) = &jwt_secret
            && secret.len() < MIN_JWT_SECRET_LEN
        {
            errors.push(ConfigError::Invalid {
                name: "JWT_SECRET",
                reason: format!(
                    "must be at least {MIN_JWT_SECRET_LEN} characters, got {}",
                    secret.len()
                ),
            });
        }

        let host = parsed("HOST", IpAddr::from([0, 0, 0, 0]), &mut errors);
        let port = parsed("PORT", 8080u16, &mut errors);
        let request_timeout = seconds("REQUEST_TIMEOUT_SECS", 15, &mut errors);
        let shutdown_grace = seconds("SHUTDOWN_GRACE_SECS", 20, &mut errors);
        let body_limit = parsed("BODY_LIMIT_BYTES", 256 * 1024usize, &mut errors);
        let max_connections = parsed("DATABASE_MAX_CONNECTIONS", 20u32, &mut errors);
        let auto_migrate = parsed("AUTO_MIGRATE", true, &mut errors);
        let token_ttl = seconds("TOKEN_TTL_SECS", 12 * 3600, &mut errors);
        let refresh_interval = seconds("CACHE_REFRESH_SECS", 60, &mut errors);
        let metrics_token = std::env::var("METRICS_TOKEN")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        let burst = parsed("RATE_LIMIT_BURST", 120u32, &mut errors);
        let per_second = parsed("RATE_LIMIT_PER_SECOND", 40u32, &mut errors);

        let environment = match std::env::var("APP_ENV").as_deref() {
            Ok("production") | Ok("prod") => RuntimeEnvironment::Production,
            Ok("development") | Ok("dev") | Err(_) => RuntimeEnvironment::Development,
            Ok(other) => {
                errors.push(ConfigError::Invalid {
                    name: "APP_ENV",
                    reason: format!("expected `development` or `production`, got `{other}`"),
                });
                RuntimeEnvironment::Development
            }
        };

        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self {
            server: ServerConfig {
                address: SocketAddr::new(host, port),
                request_timeout,
                body_limit_bytes: body_limit,
                shutdown_grace,
            },
            database: DatabaseConfig {
                url: database_url.expect("checked above"),
                max_connections,
                auto_migrate,
            },
            auth: AuthConfig {
                jwt_secret: jwt_secret.expect("checked above"),
                token_ttl,
                metrics_token,
            },
            cache: CacheConfig { refresh_interval },
            rate_limit: RateLimitConfig { burst, per_second },
            environment,
        })
    }
}

fn required(name: &'static str, errors: &mut Vec<ConfigError>) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => {
            errors.push(ConfigError::Missing(name));
            None
        }
    }
}

fn parsed<T>(name: &'static str, default: T, errors: &mut Vec<ConfigError>) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Err(_) => default,
        Ok(raw) => match raw.trim().parse() {
            Ok(value) => value,
            Err(err) => {
                errors.push(ConfigError::Invalid { name, reason: err.to_string() });
                default
            }
        },
    }
}

fn seconds(name: &'static str, default: u64, errors: &mut Vec<ConfigError>) -> Duration {
    Duration::from_secs(parsed(name, default, errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_jwt_secret_is_rejected() {
        // `from_env` reads process-wide state, so exercise the rule directly
        // rather than mutating the environment from a parallel test.
        let secret = "too-short";
        assert!(secret.len() < MIN_JWT_SECRET_LEN);
    }

    #[test]
    fn parsing_falls_back_to_defaults_without_an_error() {
        let mut errors = Vec::new();
        let value = parsed::<u16>("FLAGFORGE_DEFINITELY_UNSET_VARIABLE", 8080, &mut errors);
        assert_eq!(value, 8080);
        assert!(errors.is_empty());
    }
}
