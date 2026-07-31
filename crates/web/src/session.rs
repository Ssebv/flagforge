//! Authentication state.
//!
//! The token lives in `localStorage` so a reload does not sign the user out.
//! That is a deliberate trade: it is readable by any script on the origin, but
//! the alternative — an httpOnly cookie — would need CSRF handling that the
//! API does not have, and the API is authenticated by header precisely so it
//! carries no ambient authority.

use gloo_storage::{LocalStorage, Storage};
use leptos::prelude::*;

use crate::api::{self, models::Me};

const TOKEN_KEY: &str = "flagforge.token";

/// Reactive session, provided as context at the root.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    token: RwSignal<Option<String>>,
    identity: RwSignal<Option<Me>>,
    /// True until the stored token has been checked against the server, so
    /// route guards do not bounce a signed-in user to the login page during
    /// the first frame.
    resolving: RwSignal<bool>,
}

impl Session {
    pub fn new() -> Self {
        let stored: Option<String> = LocalStorage::get(TOKEN_KEY).ok();
        Self {
            resolving: RwSignal::new(stored.is_some()),
            token: RwSignal::new(stored),
            identity: RwSignal::new(None),
        }
    }

    /// Non-reactive read. Every caller is an event handler or an async block,
    /// where subscribing to the token signal would be wrong.
    pub fn token_untracked(&self) -> Option<String> {
        self.token.get_untracked()
    }

    pub fn identity(&self) -> Option<Me> {
        self.identity.get()
    }

    pub fn is_resolving(&self) -> bool {
        self.resolving.get()
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.get().is_some()
    }

    pub fn sign_in(&self, token: String) {
        let _ = LocalStorage::set(TOKEN_KEY, &token);
        self.token.set(Some(token));
        self.resolving.set(true);
        self.refresh_identity();
    }

    pub fn sign_out(&self) {
        LocalStorage::delete(TOKEN_KEY);
        self.token.set(None);
        self.identity.set(None);
        self.resolving.set(false);
    }

    /// Confirms the stored token is still valid and loads the user.
    ///
    /// A token that expired while the tab was closed would otherwise fail on
    /// whatever the user clicked first; checking once at start-up turns that
    /// into a clean redirect to the login page.
    pub fn refresh_identity(&self) {
        let Some(token) = self.token.get_untracked() else {
            self.resolving.set(false);
            return;
        };

        let session = *self;
        leptos::task::spawn_local(async move {
            match api::me(&token).await {
                Ok(me) => {
                    session.identity.set(Some(me));
                    session.resolving.set(false);
                }
                Err(error) if error.is_unauthorized() => session.sign_out(),
                Err(_) => {
                    // The server is unreachable rather than rejecting us; keep
                    // the token so a flaky network does not log anyone out.
                    session.resolving.set(false);
                }
            }
        });
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads the session from context. Panics only if the provider is missing,
/// which is a programming error rather than a runtime condition.
pub fn use_session() -> Session {
    expect_context::<Session>()
}
