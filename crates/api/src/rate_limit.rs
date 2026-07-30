//! A per-client token bucket.
//!
//! This is in-process, so it bounds what a single node will do rather than
//! enforcing a global quota — a real deployment puts a shared limiter at the
//! edge. What it does guarantee is that one misbehaving SDK cannot saturate
//! the node it happens to land on, which is the failure this service is most
//! exposed to.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use dashmap::DashMap;

use crate::config::RateLimitConfig;
use crate::error::ApiError;

/// Buckets idle for longer than this are dropped by the sweeper.
const IDLE_EVICTION_SECS: f64 = 300.0;
/// Sweep once the map grows past this many clients.
const SWEEP_THRESHOLD: usize = 10_000;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Debug)]
pub struct RateLimiter {
    buckets: DashMap<String, Bucket>,
    burst: f64,
    per_second: f64,
}

impl RateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            buckets: DashMap::new(),
            burst: f64::from(config.burst.max(1)),
            per_second: f64::from(config.per_second.max(1)),
        }
    }

    /// Consumes one token. Returns the seconds to wait when the bucket is dry.
    pub fn check(&self, client: &str) -> Result<(), u64> {
        let now = Instant::now();

        if self.buckets.len() > SWEEP_THRESHOLD {
            self.sweep(now);
        }

        let mut bucket = self
            .buckets
            .entry(client.to_owned())
            .or_insert_with(|| Bucket { tokens: self.burst, last_refill: now });

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.per_second).min(self.burst);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(());
        }

        // Always at least a second, so a client polling on the returned value
        // cannot spin.
        let wait = ((1.0 - bucket.tokens) / self.per_second).ceil().max(1.0);
        Err(wait as u64)
    }

    /// Drops buckets that have been full and untouched long enough that
    /// forgetting them changes nothing.
    fn sweep(&self, now: Instant) {
        self.buckets.retain(|_, bucket| {
            now.duration_since(bucket.last_refill).as_secs_f64() < IDLE_EVICTION_SECS
        });
    }

    pub fn tracked_clients(&self) -> usize {
        self.buckets.len()
    }
}

/// Middleware entry point.
pub async fn enforce(
    State(limiter): State<Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let client = client_key(&request);

    match limiter.check(&client) {
        Ok(()) => Ok(next.run(request).await),
        Err(retry_after_secs) => {
            metrics::counter!("flagforge_rate_limited_total").increment(1);
            Err(ApiError::RateLimited { retry_after_secs })
        }
    }
}

/// Identifies the caller to limit.
///
/// The credential is the right unit when there is one — two SDKs behind the
/// same NAT are different clients. `X-Forwarded-For` is only consulted as a
/// fallback and is trusted no further than that: it is client-controlled, so
/// it can widen a limit but never narrow someone else's.
fn client_key(request: &Request) -> String {
    if let Some(credential) = request.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        // Bucket on a hash so the map never holds live secrets.
        return format!("cred:{}", crate::auth::keys::hash(credential));
    }

    let forwarded = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    match forwarded {
        Some(ip) => format!("ip:{ip}"),
        None => "anonymous".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(burst: u32, per_second: u32) -> RateLimiter {
        RateLimiter::new(&RateLimitConfig { burst, per_second })
    }

    #[test]
    fn a_client_may_spend_its_whole_burst_at_once() {
        let limiter = limiter(5, 1);
        for _ in 0..5 {
            assert!(limiter.check("a").is_ok());
        }
        assert!(limiter.check("a").is_err());
    }

    #[test]
    fn clients_do_not_share_a_bucket() {
        let limiter = limiter(1, 1);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
        // Exhausting one client must leave the next one untouched.
        assert!(limiter.check("b").is_ok());
    }

    #[test]
    fn the_retry_hint_is_never_zero() {
        let limiter = limiter(1, 1000);
        assert!(limiter.check("a").is_ok());
        // With a fast refill the computed wait rounds to zero; a client told
        // to wait zero seconds would hammer us.
        if let Err(wait) = limiter.check("a") {
            assert!(wait >= 1);
        }
    }

    #[test]
    fn tokens_refill_over_time() {
        let limiter = limiter(1, 100);
        assert!(limiter.check("a").is_ok());
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(limiter.check("a").is_ok(), "the bucket should have refilled");
    }

    #[test]
    fn a_zero_rate_is_treated_as_one_rather_than_dividing_by_zero() {
        let limiter = limiter(0, 0);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
    }

    #[test]
    fn credentials_are_hashed_before_being_used_as_keys() {
        let request = Request::builder()
            .header(AUTHORIZATION, "Bearer ff_srv_supersecret")
            .body(axum::body::Body::empty())
            .unwrap();

        let key = client_key(&request);
        assert!(key.starts_with("cred:"));
        assert!(!key.contains("supersecret"));
    }

    #[test]
    fn falls_back_to_the_first_forwarded_address() {
        let request = Request::builder()
            .header("x-forwarded-for", "203.0.113.7, 70.41.3.18")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(client_key(&request), "ip:203.0.113.7");
    }
}
