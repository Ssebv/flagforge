//! The client, its builder and the background refresher.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use flagforge_core::{EnvironmentSnapshot, Evaluation, EvaluationContext, VariantValue};
use tokio::task::JoinHandle;

use crate::error::Error;

/// How often the configuration is re-fetched by default.
///
/// Polling rather than streaming: it is one moving part instead of three, and
/// thirty seconds of staleness is well inside what a rollout decision
/// tolerates. A kill switch that has to act faster than that wants
/// [`Client::refresh`] wired to your own signal.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Backoff cap for a server that is down; low enough to recover promptly,
/// high enough not to add load to something already struggling.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A point-in-time view of an environment's configuration.
pub type Snapshot = EnvironmentSnapshot;

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    url: String,
    key: String,
    /// `None` until the first successful fetch. Lock-free to read, because
    /// every flag check reads it.
    current: ArcSwapOption<Snapshot>,
}

impl Inner {
    async fn fetch(&self) -> Result<Snapshot, Error> {
        let response = self
            .http
            .get(&self.url)
            .header("authorization", format!("Bearer {}", self.key))
            .send()
            .await?;

        let status = response.status().as_u16();
        let body = response.text().await?;

        if status == 401 || status == 403 {
            return Err(Error::Unauthorized(detail(&body)));
        }
        if !(200..300).contains(&status) {
            return Err(Error::Api { status, detail: detail(&body) });
        }

        // The API's snapshot response is field-compatible with the engine's
        // own type, salt included, so there is nothing to translate.
        Ok(serde_json::from_str(&body)?)
    }

    fn store(&self, snapshot: Snapshot) {
        self.current.store(Some(Arc::new(snapshot)));
    }
}

/// Pulls the human-readable part out of an RFC 9457 problem document.
fn detail(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|problem| problem["title"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Configures a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base_url: String,
    key: String,
    poll_interval: Duration,
    request_timeout: Duration,
}

impl ClientBuilder {
    /// How often to re-fetch. Pass [`Duration::ZERO`] to disable polling and
    /// drive refreshes yourself.
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Fetches the configuration, then returns a ready client.
    ///
    /// Fails if the first fetch does — which is usually what you want at
    /// start-up, where a bad key or a wrong URL should stop a deploy rather
    /// than silently serve fallbacks forever.
    pub async fn connect(self) -> Result<Client, Error> {
        let (client, inner) = self.build()?;
        let snapshot = inner.fetch().await?;

        tracing::info!(
            environment = %snapshot.environment_key,
            flags = snapshot.len(),
            version = snapshot.version,
            "flagforge: loaded configuration"
        );
        inner.store(snapshot);

        Ok(client)
    }

    /// Returns immediately and loads in the background.
    ///
    /// Until the first fetch lands, every evaluation returns the fallback the
    /// call site passed. Use this when the service must boot even if the flag
    /// system is down.
    pub fn connect_lazy(self) -> Result<Client, Error> {
        let (client, _) = self.build()?;
        Ok(client)
    }

    fn build(self) -> Result<(Client, Arc<Inner>), Error> {
        let base = self.base_url.trim_end_matches('/');
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(Error::InvalidUrl(self.base_url));
        }

        let http = reqwest::Client::builder()
            .timeout(self.request_timeout)
            // A stuck refresh must not pin a connection forever.
            .connect_timeout(self.request_timeout)
            .build()?;

        let inner = Arc::new(Inner {
            http,
            url: format!("{base}/api/v1/snapshot"),
            key: self.key,
            current: ArcSwapOption::empty(),
        });

        let refresher = (!self.poll_interval.is_zero())
            .then(|| spawn_refresher(Arc::clone(&inner), self.poll_interval));

        Ok((Client { inner: Arc::clone(&inner), refresher: refresher.map(Arc::new) }, inner))
    }
}

/// A FlagForge client. Cheap to clone; share one per process.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
    refresher: Option<Arc<JoinHandle<()>>>,
}

impl Client {
    /// Starts configuring a client for `base_url` (the FlagForge origin, not
    /// the snapshot path) using a **server-scoped** key.
    pub fn builder(base_url: impl Into<String>, key: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
            key: key.into(),
            poll_interval: DEFAULT_POLL_INTERVAL,
            request_timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Whether a configuration has been loaded yet.
    ///
    /// Worth surfacing on your own readiness probe: a client that is still
    /// empty is answering every question with the caller's fallback.
    pub fn is_ready(&self) -> bool {
        self.inner.current.load().is_some()
    }

    /// Version of the loaded configuration, for logging alongside a decision.
    pub fn version(&self) -> Option<i64> {
        self.inner.current.load().as_ref().map(|snapshot| snapshot.version)
    }

    pub fn environment(&self) -> Option<String> {
        self.inner.current.load().as_ref().map(|s| s.environment_key.clone())
    }

    /// Re-fetches now. The background poller does this for you; call it
    /// directly to react to a webhook or a deploy hook.
    pub async fn refresh(&self) -> Result<(), Error> {
        let snapshot = self.inner.fetch().await?;
        self.inner.store(snapshot);
        Ok(())
    }

    // ------------------------------------------------------------ reading --

    /// The common case: is this flag on for this context?
    pub fn is_enabled(&self, flag: &str, context: &EvaluationContext, fallback: bool) -> bool {
        self.evaluate(flag, context, VariantValue::Bool(fallback))
            .value
            .as_bool()
            .unwrap_or(fallback)
    }

    /// A string-valued flag, for multivariate configuration.
    pub fn string_value(&self, flag: &str, context: &EvaluationContext, fallback: &str) -> String {
        self.evaluate(flag, context, VariantValue::String(fallback.to_owned()))
            .value
            .as_str()
            .unwrap_or(fallback)
            .to_owned()
    }

    pub fn number_value(&self, flag: &str, context: &EvaluationContext, fallback: f64) -> f64 {
        self.evaluate(flag, context, VariantValue::Number(fallback))
            .value
            .as_f64()
            .unwrap_or(fallback)
    }

    /// The full decision, including *why*.
    ///
    /// Log the reason next to a surprising outcome and the question "why did
    /// this user get that?" answers itself.
    pub fn evaluate(
        &self,
        flag: &str,
        context: &EvaluationContext,
        fallback: VariantValue,
    ) -> Evaluation {
        match self.inner.current.load().as_ref() {
            Some(snapshot) => snapshot.evaluate(flag, context, fallback),
            // Not loaded yet: behave exactly as if the flag did not exist.
            None => Evaluation::not_found(flag, fallback),
        }
    }

    /// Every flag in the environment, for bulk logging or a debug endpoint.
    pub fn evaluate_all(&self, context: &EvaluationContext) -> Vec<Evaluation> {
        match self.inner.current.load().as_ref() {
            Some(snapshot) => snapshot.evaluate_all(context),
            None => Vec::new(),
        }
    }

    /// Stops the background refresher. Also happens on drop.
    pub fn shutdown(&self) {
        if let Some(handle) = &self.refresher {
            handle.abort();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Only the last clone should stop the refresher; earlier ones going
        // out of scope must not silently freeze everyone else's flags.
        if let Some(handle) = &self.refresher
            && Arc::strong_count(handle) == 1
        {
            handle.abort();
        }
    }
}

fn spawn_refresher(inner: Arc<Inner>, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = interval;

        loop {
            tokio::time::sleep(backoff).await;

            match inner.fetch().await {
                Ok(snapshot) => {
                    let changed =
                        inner.current.load().as_ref().map(|s| s.version) != Some(snapshot.version);
                    if changed {
                        tracing::info!(
                            version = snapshot.version,
                            flags = snapshot.len(),
                            "flagforge: configuration changed"
                        );
                    }
                    inner.store(snapshot);
                    backoff = interval;
                }
                Err(error) => {
                    // Keep serving what we have. A flag client that starts
                    // erroring because it could not reach its server takes the
                    // whole service down with it, which is the opposite of the
                    // point.
                    tracing::warn!(
                        %error,
                        transient = error.is_transient(),
                        "flagforge: refresh failed, still serving the last configuration"
                    );

                    backoff = if error.is_transient() {
                        (backoff * 2).min(MAX_BACKOFF)
                    } else {
                        // A rejected key will not start working; stop hammering
                        // but keep checking occasionally in case it is rotated.
                        MAX_BACKOFF
                    };
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_without_a_scheme_is_rejected_before_any_request() {
        let error = Client::builder("flags.example.com", "ff_srv_x")
            .poll_interval(Duration::ZERO)
            .connect_lazy()
            .unwrap_err();
        assert!(matches!(error, Error::InvalidUrl(_)));
    }

    #[test]
    fn trailing_slashes_do_not_produce_a_double_slash() {
        // A zero interval skips the background task, so this needs no runtime.
        let (_client, inner) = Client::builder("https://flags.example.com/", "ff_srv_x")
            .poll_interval(Duration::ZERO)
            .build()
            .unwrap();
        assert_eq!(inner.url, "https://flags.example.com/api/v1/snapshot");
    }

    #[tokio::test]
    async fn an_unloaded_client_serves_the_callers_fallback() {
        let client = Client::builder("https://flags.example.com", "ff_srv_x")
            .poll_interval(Duration::ZERO)
            .connect_lazy()
            .unwrap();

        assert!(!client.is_ready());
        assert!(client.version().is_none());

        let context = EvaluationContext::new("user-1");
        assert!(client.is_enabled("anything", &context, true));
        assert!(!client.is_enabled("anything", &context, false));
        assert_eq!(client.string_value("anything", &context, "blue"), "blue");
        assert_eq!(client.number_value("anything", &context, 1.5), 1.5);
        assert!(client.evaluate_all(&context).is_empty());
    }

    #[test]
    fn a_rejected_key_is_not_worth_retrying_but_a_dropped_connection_is() {
        assert!(!Error::Unauthorized("nope".into()).is_transient());
        assert!(!Error::Api { status: 404, detail: String::new() }.is_transient());
        assert!(Error::Api { status: 503, detail: String::new() }.is_transient());
        assert!(Error::Api { status: 429, detail: String::new() }.is_transient());
    }

    #[test]
    fn problem_documents_are_unwrapped_into_a_readable_message() {
        let body = r#"{"type":"unauthorized","title":"unknown or revoked SDK key","status":401}"#;
        assert_eq!(detail(body), "unknown or revoked SDK key");
        // Anything that is not a problem document still yields something.
        assert_eq!(detail("upstream connect error"), "upstream connect error");
    }
}
