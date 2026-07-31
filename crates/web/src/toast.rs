//! Transient notifications.
//!
//! Every mutation in the dashboard reports its outcome here. Silence after a
//! click is the single most common way a UI feels broken when it is not.

use leptos::prelude::*;

use crate::api::ApiError;

const DISMISS_AFTER_MS: u32 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub level: Level,
    pub title: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Toaster {
    items: RwSignal<Vec<Toast>>,
    next_id: RwSignal<u64>,
}

impl Toaster {
    pub fn new() -> Self {
        Self { items: RwSignal::new(Vec::new()), next_id: RwSignal::new(1) }
    }

    pub fn items(&self) -> Vec<Toast> {
        self.items.get()
    }

    pub fn success(&self, title: impl Into<String>) {
        self.push(Level::Success, title.into(), None);
    }

    /// Reports a failed call, surfacing the field-level detail when the server
    /// sent one — "the configuration is invalid" alone is not actionable.
    pub fn error(&self, context: impl Into<String>, error: &ApiError) {
        let detail = match error.issues.first() {
            Some(issue) => Some(format!("{}: {}", issue.path, issue.message)),
            None => Some(error.title.clone()),
        };
        self.push(Level::Error, context.into(), detail);
    }

    pub fn message(&self, level: Level, title: impl Into<String>, detail: impl Into<String>) {
        self.push(level, title.into(), Some(detail.into()));
    }

    fn push(&self, level: Level, title: String, detail: Option<String>) {
        let id = self.next_id.get_untracked();
        self.next_id.set(id + 1);
        self.items.update(|items| items.push(Toast { id, level, title, detail }));

        let items = self.items;
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(DISMISS_AFTER_MS).await;
            items.update(|items| items.retain(|toast| toast.id != id));
        });
    }

    pub fn dismiss(&self, id: u64) {
        self.items.update(|items| items.retain(|toast| toast.id != id));
    }
}

impl Default for Toaster {
    fn default() -> Self {
        Self::new()
    }
}

pub fn use_toaster() -> Toaster {
    expect_context::<Toaster>()
}
