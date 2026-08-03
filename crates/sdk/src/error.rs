//! Errors the client can return.
//!
//! Only start-up and refresh can fail. Evaluation deliberately cannot: a
//! `Result` at every flag check would push callers into `unwrap()` on the hot
//! path, and a flag lookup that can fail is a flag lookup people stop using.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` is not a valid FlagForge URL")]
    InvalidUrl(String),

    /// The key was rejected, or it is client-scoped. Client keys can evaluate
    /// but cannot download rules, and this SDK evaluates locally — so it needs
    /// a server key.
    #[error("the SDK key was rejected: {0}")]
    Unauthorized(String),

    #[error("could not reach FlagForge: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("FlagForge returned {status}: {detail}")]
    Api { status: u16, detail: String },

    #[error("could not read the snapshot FlagForge returned: {0}")]
    Malformed(#[from] serde_json::Error),
}

impl Error {
    /// Whether retrying could plausibly succeed.
    ///
    /// A rejected key will still be rejected in thirty seconds; a connection
    /// reset might not be. The background refresher uses this to decide
    /// whether to keep trying or to stop and say so loudly.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Api { status, .. } => *status >= 500 || *status == 429,
            Self::InvalidUrl(_) | Self::Unauthorized(_) | Self::Malformed(_) => false,
        }
    }
}
