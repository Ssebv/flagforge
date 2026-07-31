//! Light/dark theme.
//!
//! `index.html` sets `data-theme` before the first paint; this only handles
//! changes made after the app has mounted, and persists the choice so it is
//! not re-derived from the OS on the next visit.

use leptos::prelude::*;

const THEME_KEY: &str = "flagforge.theme";

/// Writes the preference as a bare string.
///
/// Deliberately not `gloo_storage`, which JSON-encodes and would store
/// `"dark"` *including the quotes*. The pre-paint script in `index.html` is
/// plain JavaScript comparing against `dark`, so the two have to agree on the
/// raw format — otherwise the theme silently resets on every reload.
fn store(value: &str) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(THEME_KEY, value);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

/// Reads whatever the pre-paint script decided, so the two can never disagree.
pub fn current() -> Theme {
    let attribute = document()
        .document_element()
        .and_then(|root| root.get_attribute("data-theme"))
        .unwrap_or_default();

    if attribute == "dark" { Theme::Dark } else { Theme::Light }
}

pub fn toggle(signal: RwSignal<Theme>) {
    let next = signal.get_untracked().toggled();
    signal.set(next);

    if let Some(root) = document().document_element() {
        let _ = root.set_attribute("data-theme", next.as_str());
    }
    store(next.as_str());
}
