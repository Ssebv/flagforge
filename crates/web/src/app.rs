//! Application shell and routing.

use leptos::prelude::*;
use leptos_router::components::{A, Outlet, ParentRoute, Route, Router, Routes};
use leptos_router::hooks::use_navigate;
use leptos_router::path;

use crate::components::{Icon, defer};
use crate::pages;
use crate::session::{Session, use_session};
use crate::theme::{self, Theme};
use crate::toast::{Level, Toaster, use_toaster};

#[component]
pub fn App() -> impl IntoView {
    provide_context(Session::new());
    provide_context(Toaster::new());
    provide_context(RwSignal::new(theme::current()));

    // Confirm a stored token is still good before the first guarded route
    // renders, so a returning user is not bounced to the login page.
    use_session().refresh_identity();

    view! {
        <Router>
            <Routes fallback=|| view! { <pages::NotFound /> }>
                <Route path=path!("/login") view=pages::Login />

                <ParentRoute path=path!("") view=Shell>
                    <Route path=path!("") view=pages::Projects />
                    <Route path=path!("projects") view=pages::Projects />
                    <Route path=path!("projects/:project") view=pages::ProjectDetail />
                    <Route path=path!("projects/:project/flags/:flag") view=pages::FlagDetail />
                    <Route path=path!("projects/:project/keys") view=pages::Keys />
                    <Route path=path!("audit") view=pages::Audit />
                </ParentRoute>
            </Routes>
        </Router>

        <Toasts />
    }
}

/// Sidebar + content frame for every authenticated page.
#[component]
fn Shell() -> impl IntoView {
    let session = use_session();
    let navigate = use_navigate();

    // One guard for the whole authenticated area rather than a check per page.
    Effect::new(move |_| {
        if !session.is_resolving() && !session.is_authenticated() {
            navigate("/login", Default::default());
        }
    });

    view! {
        <Show
            when=move || session.is_authenticated()
            fallback=|| view! { <div class="boot"><div class="boot__mark"></div></div> }
        >
            <div class="shell">
                <Sidebar />
                <div class="main">
                    <Outlet />
                </div>
            </div>
        </Show>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let session = use_session();
    let theme = expect_context::<RwSignal<Theme>>();
    let navigate = use_navigate();

    let sign_out = move |_| {
        session.sign_out();
        navigate("/login", Default::default());
    };

    view! {
        <aside class="sidebar">
            <A href="/projects" attr:class="brand">
                <span class="brand__mark">"F"</span>
                <span>"FlagForge"</span>
            </A>

            <nav class="nav" aria-label="Main">
                <span class="nav__heading">"Workspace"</span>
                <A href="/projects" attr:class="nav__link">
                    <Icon name="folder" />
                    "Projects"
                </A>
                <A href="/audit" attr:class="nav__link">
                    <Icon name="history" />
                    "Audit log"
                </A>
            </nav>

            <div class="sidebar__footer">
                <div class="identity">
                    <span class="avatar">
                        {move || {
                            session.identity().map(|me| me.user.initials()).unwrap_or_default()
                        }}
                    </span>
                    <span class="identity__text">
                        <span class="identity__name">
                            {move || {
                                session.identity().map(|me| me.user.email).unwrap_or_default()
                            }}
                        </span>
                        <span class="identity__org">
                            {move || {
                                session
                                    .identity()
                                    .map(|me| format!(
                                        "{} · {}",
                                        me.organization.name,
                                        me.user.role,
                                    ))
                                    .unwrap_or_default()
                            }}
                        </span>
                    </span>
                </div>

                <div class="row" style="gap:var(--space-2)">
                    <button
                        class="btn btn--ghost btn--sm"
                        type="button"
                        // The accessible name has to contain the visible text
                        // (WCAG 2.5.3): a voice user says what they can see,
                        // and "Toggle colour theme" would not match "Dark".
                        aria-label=move || match theme.get() {
                            Theme::Dark => "Switch to light theme",
                            Theme::Light => "Switch to dark theme",
                        }
                        on:click=move |_| theme::toggle(theme)
                    >
                        {move || match theme.get() {
                            Theme::Dark => view! { <Icon name="sun" /> },
                            Theme::Light => view! { <Icon name="moon" /> },
                        }}
                        {move || match theme.get() {
                            Theme::Dark => "Light",
                            Theme::Light => "Dark",
                        }}
                    </button>
                    <button class="btn btn--ghost btn--sm" type="button" on:click=sign_out>
                        <Icon name="logout" />
                        "Sign out"
                    </button>
                </div>
            </div>
        </aside>
    }
}

#[component]
fn Toasts() -> impl IntoView {
    let toaster = use_toaster();

    view! {
        <div class="toasts" aria-live="polite" role="status">
            <For each=move || toaster.items() key=|toast| toast.id let:toast>
                <div class=match toast.level {
                    Level::Success => "toast toast--success",
                    Level::Error => "toast toast--error",
                }>
                    <div class="toast__body">
                        <div class="toast__title">{toast.title.clone()}</div>
                        {toast
                            .detail
                            .clone()
                            .map(|detail| view! { <div class="toast__detail">{detail}</div> })}
                    </div>
                    <button
                        class="btn btn--ghost btn--icon btn--sm"
                        type="button"
                        aria-label="Dismiss"
                        on:click=move |_| defer(move || toaster.dismiss(toast.id))
                    >
                        <Icon name="close" />
                    </button>
                </div>
            </For>
        </div>
    }
}

/// Page header, shared by every route so titles and actions line up.
///
/// Both texts are signals because half the pages title themselves from a route
/// parameter, and a header that only supported static strings would push those
/// pages into duplicating the markup.
#[component]
pub fn PageHeader(
    title: Signal<String>,
    #[prop(optional)] lead: Option<Signal<String>>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <header class="topbar">
            <div class="topbar__title">
                <h1>{move || title.get()}</h1>
                {lead.map(|lead| view! { <p class="page-lead">{move || lead.get()}</p> })}
            </div>
            <div class="topbar__actions">{children.map(|children| children())}</div>
        </header>
    }
}

/// Wraps a fixed string as a signal, for headers whose title never changes.
pub fn fixed(text: &'static str) -> Signal<String> {
    Signal::derive(move || text.to_owned())
}
