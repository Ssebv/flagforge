//! Shared UI pieces.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::api::ApiError;
use crate::toast::use_toaster;

/// Inline SVG icons.
///
/// Bundled as path data rather than fetched: an icon font or sprite request
/// would be one more thing that can fail to load, for a few hundred bytes.
#[component]
pub fn Icon(name: &'static str) -> impl IntoView {
    view! {
        <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            inner_html=path_for(name)
        ></svg>
    }
}

fn path_for(name: &str) -> &'static str {
    match name {
        "flag" => r#"<path d="M4 21V4h13l-3 4.5L17 13H4"/>"#,
        "folder" => {
            r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        }
        "key" => {
            r#"<circle cx="8" cy="14" r="4"/><path d="M11 11 20 2m-3 3 2.5 2.5M14.5 7.5 17 10"/>"#
        }
        "history" => {
            r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l3.5 2"/>"#
        }
        "layers" => r#"<path d="M12 3 3 8l9 5 9-5z"/><path d="m3 13 9 5 9-5"/>"#,
        "sun" => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M2 12h2m16 0h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>"#
        }
        "moon" => r#"<path d="M20 14.5A8.5 8.5 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5z"/>"#,
        "plus" => r#"<path d="M12 5v14M5 12h14"/>"#,
        "trash" => {
            r#"<path d="M4 7h16M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2m2 0v12a1 1 0 0 1-1 1H8a1 1 0 0 1-1-1V7"/>"#
        }
        "check" => r#"<path d="m5 12.5 4.5 4.5L19 7"/>"#,
        "close" => r#"<path d="M6 6 18 18M18 6 6 18"/>"#,
        "alert" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7.5v5M12 16.2v.1"/>"#,
        "copy" => {
            r#"<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>"#
        }
        "back" => r#"<path d="M15 5 8 12l7 7"/>"#,
        "logout" => {
            r#"<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="M16 17l5-5-5-5M21 12H9"/>"#
        }
        "search" => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#,
        "inbox" => {
            r#"<path d="M3 12h5l2 3h4l2-3h5"/><path d="M5 5h14l2 7v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-6z"/>"#
        }
        _ => r#"<circle cx="12" cy="12" r="9"/>"#,
    }
}

/// Runs an action after the browser has finished dispatching the current event.
///
/// Tearing a subtree down from inside a click handler drops the event listeners
/// of the nodes the event is still bubbling through, and wasm-bindgen reports
/// that as "closure invoked recursively or after being dropped". Every control
/// that dismisses its own container — a modal's close button, a toast's dismiss
/// — has to defer the state change past the end of dispatch.
///
/// A microtask is *not* enough: the HTML spec runs a microtask checkpoint after
/// each listener returns, so a queued microtask still fires part-way up the
/// bubble path. A zero-delay timer is a macrotask and therefore runs only once
/// dispatch has finished.
pub fn defer(action: impl FnOnce() + 'static) {
    leptos::task::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(0).await;
        action();
    });
}

/// An accessible on/off control.
#[component]
pub fn Switch(
    #[prop(into)] checked: Signal<bool>,
    #[prop(into)] on_change: Callback<bool>,
    #[prop(into)] label: String,
    #[prop(into, optional)] disabled: Signal<bool>,
) -> impl IntoView {
    view! {
        <button
            type="button"
            class="switch"
            role="switch"
            aria-checked=move || checked.get().to_string()
            aria-label=label
            disabled=move || disabled.get()
            on:click=move |_| on_change.run(!checked.get_untracked())
        ></button>
    }
}

/// Everything a keyboard can land on. Used to work out where a trapped Tab
/// should wrap to.
const FOCUSABLE: &str = concat!(
    "a[href], button:not([disabled]), input:not([disabled]), ",
    "select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])"
);

/// A dialog.
///
/// Closes on Escape and on a backdrop click, because a modal that can only be
/// dismissed by finding the right button is a trap. It is also a focus trap in
/// the deliberate sense: `aria-modal` tells a screen reader that the rest of
/// the page is inert, so if Tab can still reach the page behind it, the two
/// disagree and a keyboard user ends up typing into a form they cannot see.
#[component]
pub fn Modal(
    #[prop(into)] title: String,
    #[prop(into)] on_close: Callback<()>,
    children: Children,
) -> impl IntoView {
    let close = move || defer(move || on_close.run(()));
    let heading = title.clone();
    let dialog = NodeRef::<leptos::html::Div>::new();
    // Whatever had focus when the dialog opened, so it can be handed back.
    let opener = StoredValue::new(None::<web_sys::HtmlElement>);

    let handle = window_event_listener(ev::keydown, move |event| {
        if event.key() == "Escape" {
            defer(move || on_close.run(()));
        }
    });

    Effect::new(move |_| {
        let Some(node) = dialog.get() else { return };

        opener.set_value(
            document()
                .active_element()
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok()),
        );

        // Focus the first control rather than the dialog itself: the point of
        // opening a form is to type in it.
        if let Some(first) = focusable_within(&node).first() {
            let _ = first.focus();
        }
    });

    on_cleanup(move || {
        handle.remove();
        // Returning focus is what makes a dialog feel like a detour rather
        // than a dead end — without it, the next Tab starts from the top of
        // the document.
        if let Some(element) = opener.get_value() {
            let _ = element.focus();
        }
    });

    let trap = move |event: web_sys::KeyboardEvent| {
        if event.key() != "Tab" {
            return;
        }
        let Some(node) = dialog.get_untracked() else { return };
        let focusable = focusable_within(&node);
        let (Some(first), Some(last)) = (focusable.first(), focusable.last()) else {
            return;
        };

        let active = document().active_element();
        let at_edge = |edge: &web_sys::HtmlElement| {
            active.as_ref().is_some_and(|element| element.is_same_node(Some(edge)))
        };

        if event.shift_key() && at_edge(first) {
            event.prevent_default();
            let _ = last.focus();
        } else if !event.shift_key() && at_edge(last) {
            event.prevent_default();
            let _ = first.focus();
        }
    };

    view! {
        <div class="backdrop" role="presentation" on:click=move |_| close()>
            <div
                class="modal"
                role="dialog"
                aria-modal="true"
                aria-label=title
                node_ref=dialog
                // Without this, a click anywhere inside the dialog bubbles to
                // the backdrop and closes it mid-edit.
                on:click=|event| event.stop_propagation()
                on:keydown=trap
            >
                <div class="card__header">
                    <h2 class="card__title">{heading}</h2>
                    <div class="spacer"></div>
                    <button
                        class="btn btn--ghost btn--icon"
                        type="button"
                        aria-label="Close"
                        on:click=move |_| close()
                    >
                        <Icon name="close" />
                    </button>
                </div>
                {children()}
            </div>
        </div>
    }
}

/// Focusable descendants in document order, skipping anything not rendered.
fn focusable_within(root: &web_sys::HtmlElement) -> Vec<web_sys::HtmlElement> {
    let Ok(nodes) = root.query_selector_all(FOCUSABLE) else {
        return Vec::new();
    };

    (0..nodes.length())
        .filter_map(|i| nodes.item(i))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        // `offset_parent` is None for anything `display: none`, which is the
        // cheap way to skip controls inside a collapsed section.
        .filter(|element| element.offset_parent().is_some())
        .collect()
}

/// Shown when a list has no items yet.
#[component]
pub fn Empty(
    #[prop(into)] icon: &'static str,
    #[prop(into)] title: String,
    #[prop(into)] text: String,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="empty">
            <div class="empty__icon">
                <div style="width:20px;height:20px">
                    <Icon name=icon />
                </div>
            </div>
            <p class="empty__title">{title}</p>
            <p class="empty__text">{text}</p>
            {children.map(|children| children())}
        </div>
    }
}

/// Shown when a fetch failed. Always offers a way forward rather than just
/// stating the problem.
#[component]
pub fn Failure(error: ApiError, #[prop(into)] on_retry: Callback<()>) -> impl IntoView {
    view! {
        <div class="empty">
            <div class="empty__icon" style="color:var(--danger)">
                <div style="width:20px;height:20px">
                    <Icon name="alert" />
                </div>
            </div>
            <p class="empty__title">"Could not load this"</p>
            <p class="empty__text">{error.title}</p>
            <button class="btn btn--secondary" on:click=move |_| on_retry.run(())>
                "Try again"
            </button>
        </div>
    }
}

/// Placeholder rows, sized like the table they stand in for so the layout does
/// not jump when the data arrives.
#[component]
pub fn SkeletonRows(#[prop(default = 3)] rows: usize) -> impl IntoView {
    view! {
        <div style="padding:var(--space-5);display:flex;flex-direction:column;gap:var(--space-4)">
            {(0..rows)
                .map(|i| {
                    let width = 55 + (i * 13) % 35;
                    view! {
                        <div style="display:flex;align-items:center;gap:var(--space-4)">
                            <div class="skeleton" style="height:14px;flex:1;max-width:none"
                                style:width=format!("{width}%") />
                            <div class="skeleton" style="height:22px;width:38px;border-radius:999px" />
                        </div>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// Copies text to the clipboard and says so.
#[component]
pub fn CopyButton(#[prop(into)] value: String, #[prop(into)] label: String) -> impl IntoView {
    let toaster = use_toaster();

    view! {
        <button
            class="btn btn--secondary btn--sm"
            type="button"
            on:click=move |_| {
                let value = value.clone();
                let label = label.clone();
                leptos::task::spawn_local(async move {
                    let clipboard = window().navigator().clipboard();
                    match wasm_bindgen_futures::JsFuture::from(clipboard.write_text(&value)).await {
                        Ok(_) => toaster.success(format!("{label} copied")),
                        Err(_) => {
                            toaster
                                .message(
                                    crate::toast::Level::Error,
                                    "Could not copy",
                                    "Your browser blocked clipboard access — select and copy manually.",
                                )
                        }
                    }
                });
            }
        >
            <Icon name="copy" />
            "Copy"
        </button>
    }
}

/// A destructive action that asks first.
///
/// Two-step rather than a confirm dialog: it keeps the decision next to the
/// thing being deleted, and `window.confirm` would block the whole page.
#[component]
pub fn ConfirmButton(
    #[prop(into)] label: String,
    #[prop(into)] confirm_label: String,
    #[prop(into)] on_confirm: Callback<()>,
) -> impl IntoView {
    let armed = RwSignal::new(false);

    view! {
        <Show
            when=move || armed.get()
            fallback=move || {
                let label = label.clone();
                view! {
                    <button
                        class="btn btn--danger btn--sm"
                        type="button"
                        on:click=move |_| armed.set(true)
                    >
                        {label}
                    </button>
                }
            }
        >
            <span class="row" style="gap:var(--space-2)">
                <button
                    class="btn btn--danger btn--sm"
                    type="button"
                    on:click=move |_| {
                        armed.set(false);
                        on_confirm.run(());
                    }
                >
                    {confirm_label.clone()}
                </button>
                <button
                    class="btn btn--ghost btn--sm"
                    type="button"
                    on:click=move |_| armed.set(false)
                >
                    "Cancel"
                </button>
            </span>
        </Show>
    }
}

/// Relative time, so "2 minutes ago" beats an ISO timestamp in a table.
pub fn relative_time(timestamp: &str) -> String {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return timestamp.to_owned();
    };

    let seconds = (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc)).num_seconds();

    match seconds {
        s if s < 0 => "just now".to_owned(),
        s if s < 60 => "just now".to_owned(),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s if s < 2_592_000 => format!("{}d ago", s / 86_400),
        _ => parsed.format("%d %b %Y").to_string(),
    }
}
