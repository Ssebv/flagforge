//! FlagForge dashboard.
//!
//! A client-side Leptos app compiled to WebAssembly and embedded in the API
//! binary, so the whole product still ships as one container with no Node in
//! the build.

mod api;
mod app;
mod components;
mod pages;
mod session;
mod theme;
mod toast;

use leptos::prelude::*;

fn main() {
    // Turns a WASM panic into a readable stack trace in the console instead of
    // the default "unreachable executed".
    console_error_panic_hook::set_once();

    // The boot placeholder in index.html exists so a slow WASM download does
    // not look like a blank broken page; remove it now that we can paint.
    if let Some(boot) = document().get_element_by_id("boot") {
        boot.remove();
    }

    mount_to_body(app::App);
}
