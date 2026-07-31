//! Sign in and sign up.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::api::{self, LoginBody, RegisterBody};
use crate::components::Icon;
use crate::session::use_session;
use crate::theme::{self, Theme};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    SignIn,
    SignUp,
}

#[component]
pub fn Login() -> impl IntoView {
    let session = use_session();
    let navigate = use_navigate();
    let theme = expect_context::<RwSignal<Theme>>();

    let mode = RwSignal::new(Mode::SignIn);
    let organization = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(Option::<String>::None);
    let busy = RwSignal::new(false);

    // Someone who is already signed in has no business on this page.
    {
        let navigate = use_navigate();
        Effect::new(move |_| {
            if session.is_authenticated() && !session.is_resolving() {
                navigate("/projects", Default::default());
            }
        });
    }

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }

        busy.set(true);
        error.set(None);

        let navigate = navigate.clone();
        let (org, mail, pass) =
            (organization.get_untracked(), email.get_untracked(), password.get_untracked());
        let mode = mode.get_untracked();

        leptos::task::spawn_local(async move {
            let result = match mode {
                Mode::SignIn => api::login(LoginBody { email: &mail, password: &pass }).await,
                Mode::SignUp => {
                    api::register(RegisterBody {
                        organization_name: &org,
                        email: &mail,
                        password: &pass,
                    })
                    .await
                }
            };

            busy.set(false);
            match result {
                Ok(auth) => {
                    session.sign_in(auth.token);
                    navigate("/projects", Default::default());
                }
                Err(failure) => error.set(Some(failure.title)),
            }
        });
    };

    view! {
        <main class="auth">
            <div class="auth__panel">
                <div class="auth__brand">
                    <span class="brand__mark" style="width:32px;height:32px">
                        "F"
                    </span>
                    "FlagForge"
                </div>
                <p class="auth__tagline" style="margin-bottom:var(--space-5)">
                    "Feature flags with targeted rollouts and an audit trail."
                </p>

                <div class="card">
                    <div class="tabs" role="tablist">
                        <button
                            class="tab"
                            role="tab"
                            type="button"
                            aria-selected=move || (mode.get() == Mode::SignIn).to_string()
                            on:click=move |_| {
                                mode.set(Mode::SignIn);
                                error.set(None);
                            }
                        >
                            "Sign in"
                        </button>
                        <button
                            class="tab"
                            role="tab"
                            type="button"
                            aria-selected=move || (mode.get() == Mode::SignUp).to_string()
                            on:click=move |_| {
                                mode.set(Mode::SignUp);
                                error.set(None);
                            }
                        >
                            "Create an organization"
                        </button>
                    </div>

                    <form class="card__body stack" on:submit=submit>
                        <Show when=move || mode.get() == Mode::SignUp>
                            <div class="field">
                                <label class="label" for="org">
                                    "Organization name"
                                </label>
                                <input
                                    id="org"
                                    class="input"
                                    type="text"
                                    required=true
                                    autocomplete="organization"
                                    placeholder="Acme Inc"
                                    prop:value=move || organization.get()
                                    on:input=move |e| organization.set(event_target_value(&e))
                                />
                            </div>
                        </Show>

                        <div class="field">
                            <label class="label" for="email">
                                "Email"
                            </label>
                            <input
                                id="email"
                                class="input"
                                type="email"
                                required=true
                                autocomplete="email"
                                placeholder="you@company.com"
                                prop:value=move || email.get()
                                on:input=move |e| email.set(event_target_value(&e))
                            />
                        </div>

                        <div class="field">
                            <label class="label" for="password">
                                "Password"
                            </label>
                            <input
                                id="password"
                                class="input"
                                type="password"
                                required=true
                                autocomplete=move || match mode.get() {
                                    Mode::SignIn => "current-password",
                                    Mode::SignUp => "new-password",
                                }
                                prop:value=move || password.get()
                                on:input=move |e| password.set(event_target_value(&e))
                            />
                            <Show when=move || mode.get() == Mode::SignUp>
                                <span class="hint">"At least 12 characters."</span>
                            </Show>
                        </div>

                        {move || {
                            error
                                .get()
                                .map(|message| {
                                    view! {
                                        <div class="callout callout--danger" role="alert">
                                            <div style="width:16px;height:16px;flex:none">
                                                <Icon name="alert" />
                                            </div>
                                            <span>{message}</span>
                                        </div>
                                    }
                                })
                        }}

                        <button
                            class="btn btn--primary"
                            type="submit"
                            disabled=move || busy.get()
                            style="width:100%;justify-content:center"
                        >
                            <Show when=move || busy.get()>
                                <span class="spinner"></span>
                            </Show>
                            {move || match mode.get() {
                                Mode::SignIn => "Sign in",
                                Mode::SignUp => "Create organization",
                            }}
                        </button>
                    </form>
                </div>

                <div class="row" style="justify-content:center;margin-top:var(--space-4)">
                    <button
                        class="btn btn--ghost btn--sm"
                        type="button"
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
                            Theme::Dark => "Light theme",
                            Theme::Light => "Dark theme",
                        }}
                    </button>
                </div>
            </div>
        </main>
    }
}
