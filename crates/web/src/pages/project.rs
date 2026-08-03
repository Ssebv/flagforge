//! A project's flags, seen through one environment.

use flagforge_core::{Distribution, TOTAL_WEIGHT};
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{
    self, ConfigBody, Load, NewEnvironment, NewFlag,
    models::{ConfiguredFlag, Environment},
};
use crate::app::{PageHeader, fixed};
use crate::components::{
    ConfirmButton, Empty, Failure, Icon, Modal, SkeletonRows, Switch, defer, relative_time,
};
use crate::session::use_session;
use crate::toast::use_toaster;

#[component]
pub fn ProjectDetail() -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();
    let params = use_params_map();

    let project_key = move || params.read().get("project").unwrap_or_default();

    let environments = RwSignal::new(Load::<Vec<Environment>>::Loading);
    let selected = RwSignal::new(Option::<String>::None);
    let flags = RwSignal::new(Load::<Vec<ConfiguredFlag>>::Loading);
    let creating_flag = RwSignal::new(false);
    let creating_env = RwSignal::new(false);
    let filter = RwSignal::new(String::new());

    let load_environments = move || {
        let (Some(token), key) = (session.token_untracked(), project_key()) else { return };
        environments.set(Load::Loading);
        leptos::task::spawn_local(async move {
            let loaded = api::list_environments(&token, &key).await;
            if let Ok(list) = &loaded {
                // Default to production when there is one: it is the
                // environment whose state people actually need to know.
                let default = list
                    .iter()
                    .find(|e| e.is_production)
                    .or_else(|| list.first())
                    .map(|e| e.key.clone());
                selected.set(default);
            }
            environments.set(loaded.into());
        });
    };

    let load_flags = move || {
        let (Some(token), key) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected.get_untracked() else {
            flags.set(Load::Ready(Vec::new()));
            return;
        };
        flags.set(Load::Loading);
        leptos::task::spawn_local(async move {
            flags.set(api::list_configured(&token, &key, &environment).await.into());
        });
    };

    // Re-runs when the route changes, so navigating between projects reloads.
    Effect::new(move |_| {
        let _ = project_key();
        load_environments();
    });

    // Re-runs whenever the environment selection changes.
    Effect::new(move |_| {
        let _ = selected.get();
        load_flags();
    });

    let can_write = move || session.identity().is_some_and(|me| me.user.can_write());
    let can_administer = move || session.identity().is_some_and(|me| me.user.can_administer());

    let delete_project = Callback::new({
        let navigate = leptos_router::hooks::use_navigate();
        move |_| {
            let (Some(token), key) = (session.token_untracked(), project_key()) else { return };
            let navigate = navigate.clone();
            leptos::task::spawn_local(async move {
                match api::delete_project(&token, &key).await {
                    Ok(()) => {
                        toaster.success(format!("Project “{key}” deleted"));
                        navigate("/projects", Default::default());
                    }
                    Err(error) => toaster.error("Could not delete the project", &error),
                }
            });
        }
    });

    // Optimistic toggle: the switch moves immediately and rolls back if the
    // write is rejected. A flag switch that waits for a round trip feels
    // broken even when it is working.
    let toggle = move |entry: ConfiguredFlag| {
        let (Some(token), key) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected.get_untracked() else { return };
        let next = !entry.config.enabled;
        let flag_key = entry.flag.key.clone();

        flags.update(|state| {
            if let Load::Ready(list) = state
                && let Some(row) = list.iter_mut().find(|r| r.flag.key == flag_key)
            {
                row.config.enabled = next;
            }
        });

        leptos::task::spawn_local(async move {
            let result = api::put_config(
                &token,
                &key,
                &environment,
                &flag_key,
                ConfigBody {
                    enabled: next,
                    off_variant: entry.config.off_variant.clone(),
                    fallthrough: entry.config.fallthrough.clone(),
                    rules: entry.config.rules.clone(),
                    expected_version: entry.config.version,
                },
            )
            .await;

            match result {
                Ok(config) => {
                    flags.update(|state| {
                        if let Load::Ready(list) = state
                            && let Some(row) = list.iter_mut().find(|r| r.flag.key == flag_key)
                        {
                            row.config = config;
                        }
                    });
                    toaster.success(format!(
                        "{flag_key} is now {} in {environment}",
                        if next { "on" } else { "off" }
                    ));
                }
                Err(error) => {
                    // Roll back, then reload: someone else changed this flag
                    // and the version we held is stale.
                    load_flags();
                    if error.is_conflict() {
                        toaster.message(
                            crate::toast::Level::Error,
                            "Someone else changed this flag",
                            "Reloaded the latest configuration — try again.",
                        );
                    } else {
                        toaster.error("Could not change the flag", &error);
                    }
                }
            }
        });
    };

    view! {
        <PageHeader
            title=Signal::derive(project_key)
            lead=fixed("Flags and how they behave here.")
        >
            <Show when=move || selected.get().is_some()>
                <A
                    href=move || format!("/projects/{}/keys", project_key())
                    attr:class="btn btn--secondary"
                >
                    <Icon name="key" />
                    "SDK keys"
                </A>
            </Show>
            <Show when=can_write>
                <button class="btn btn--primary" on:click=move |_| creating_flag.set(true)>
                    <Icon name="plus" />
                    "New flag"
                </button>
            </Show>
        </PageHeader>

        <div class="content stack">
            <div class="row row--between">
                <EnvironmentPicker
                    environments=environments
                    selected=selected
                    on_add=Callback::new(move |_| creating_env.set(true))
                    can_write=Signal::derive(can_write)
                />

                <div class="field" style="max-width:260px;width:100%">
                    <label class="sr-only" for="flag-filter">
                        "Filter flags"
                    </label>
                    <input
                        id="flag-filter"
                        class="input"
                        type="search"
                        placeholder="Filter flags…"
                        prop:value=move || filter.get()
                        on:input=move |e| filter.set(event_target_value(&e))
                    />
                </div>
            </div>

            {move || match flags.get() {
                Load::Loading => {
                    view! { <div class="card"><SkeletonRows rows=4 /></div> }.into_any()
                }
                Load::Failed(error) => {
                    view! {
                        <div class="card">
                            <Failure error=error on_retry=Callback::new(move |_| load_flags()) />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) => {
                    let needle = filter.get().to_lowercase();
                    let visible: Vec<_> = list
                        .into_iter()
                        .filter(|entry| {
                            needle.is_empty()
                                || entry.flag.key.to_lowercase().contains(&needle)
                                || entry.flag.name.to_lowercase().contains(&needle)
                        })
                        .collect();

                    if visible.is_empty() {
                        return view! {
                            <div class="card">
                                <Empty
                                    icon="flag"
                                    title=if filter.get().is_empty() {
                                        "No flags yet"
                                    } else {
                                        "Nothing matches that filter"
                                    }
                                    text="A flag starts off in every environment, so creating one changes nothing until you turn it on."
                                />
                            </div>
                        }
                            .into_any();
                    }

                    view! {
                        <div class="card">
                            <div class="table__scroll">
                                <table class="table">
                                    <thead>
                                        <tr>
                                            <th>"Flag"</th>
                                            <th>"Serving"</th>
                                            <th>"Targeting"</th>
                                            <th>"Updated"</th>
                                            <th style="text-align:right">"Enabled"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {visible
                                            .into_iter()
                                            .map(|entry| {
                                                view! {
                                                    <FlagRow
                                                        entry=entry
                                                        project=Signal::derive(project_key)
                                                        can_write=Signal::derive(can_write)
                                                        on_toggle=Callback::new(toggle)
                                                    />
                                                }
                                            })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        </div>
                    }
                        .into_any()
                }
            }}
            <Show when=can_administer>
                <div class="card">
                    <div class="card__header">
                        <h2 class="card__title">"Danger zone"</h2>
                    </div>
                    <div class="card__body row row--between">
                        <div>
                            <div style="font-weight:540">"Delete this project"</div>
                            <span class="hint">
                                "Removes every environment, flag and SDK key it contains. Cannot be undone."
                            </span>
                        </div>
                        <ConfirmButton
                            label="Delete project"
                            confirm_label="Delete permanently"
                            on_confirm=delete_project
                        />
                    </div>
                </div>
            </Show>
        </div>

        <Show when=move || creating_flag.get()>
            <CreateFlag
                project=Signal::derive(project_key)
                on_close=Callback::new(move |_| defer(move || creating_flag.set(false)))
                on_created=Callback::new(move |key: String| {
                    creating_flag.set(false);
                    toaster.success(format!("Flag “{key}” created, off everywhere"));
                    load_flags();
                })
            />
        </Show>

        <Show when=move || creating_env.get()>
            <CreateEnvironment
                project=Signal::derive(project_key)
                on_close=Callback::new(move |_| defer(move || creating_env.set(false)))
                on_created=Callback::new(move |key: String| {
                    creating_env.set(false);
                    toaster.success(format!("Environment “{key}” created"));
                    load_environments();
                })
            />
        </Show>
    }
}

#[component]
fn EnvironmentPicker(
    environments: RwSignal<Load<Vec<Environment>>>,
    selected: RwSignal<Option<String>>,
    #[prop(into)] on_add: Callback<()>,
    can_write: Signal<bool>,
) -> impl IntoView {
    view! {
        <div class="row" style="gap:var(--space-2)">
            {move || match environments.get() {
                Load::Ready(list) if !list.is_empty() => {
                    view! {
                        <div class="row" style="gap:var(--space-1)" role="tablist">
                            {list
                                .into_iter()
                                .map(|environment| {
                                    let key = environment.key.clone();
                                    let is_active = {
                                        let key = key.clone();
                                        Signal::derive(move || {
                                            selected.get().as_deref() == Some(key.as_str())
                                        })
                                    };
                                    let choose = {
                                        let key = key.clone();
                                        move |_| selected.set(Some(key.clone()))
                                    };
                                    view! {
                                        <button
                                            class="btn btn--sm"
                                            role="tab"
                                            type="button"
                                            aria-selected=move || is_active.get().to_string()
                                            class:btn--primary=move || is_active.get()
                                            class:btn--secondary=move || !is_active.get()
                                            on:click=choose
                                        >
                                            <Show when=move || environment.is_production>
                                                <span class="dot"></span>
                                            </Show>
                                            {environment.name.clone()}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(_) => {
                    view! {
                        <span class="cell-secondary">
                            "No environments yet — add one to configure flags."
                        </span>
                    }
                        .into_any()
                }
                _ => {
                    view! { <div class="skeleton" style="height:26px;width:190px"></div> }
                        .into_any()
                }
            }}

            <Show when=move || can_write.get()>
                <button
                    class="btn btn--ghost btn--sm"
                    type="button"
                    on:click=move |_| on_add.run(())
                >
                    <Icon name="plus" />
                    "Environment"
                </button>
            </Show>
        </div>
    }
}

#[component]
fn FlagRow(
    entry: ConfiguredFlag,
    project: Signal<String>,
    can_write: Signal<bool>,
    #[prop(into)] on_toggle: Callback<ConfiguredFlag>,
) -> impl IntoView {
    let for_toggle = entry.clone();
    let toggle_label = format!("Toggle {}", entry.flag.key);
    let enabled = entry.config.enabled;
    let rules = entry.config.rules.len();
    let key = entry.flag.key.clone();
    let href = move || format!("/projects/{}/flags/{}", project.get(), key);

    view! {
        <tr>
            <td>
                <A href=href attr:style="color:inherit">
                    <div class="cell-key">{entry.flag.key.clone()}</div>
                    <div class="cell-secondary">{entry.flag.name.clone()}</div>
                </A>
            </td>

            <td>
                <span class=if enabled { "badge badge--on" } else { "badge badge--off" }>
                    <span class="dot"></span>
                    {if enabled { "On" } else { "Off" }}
                </span>
            </td>

            <td class="cell-secondary">
                {describe_targeting(
                    rules,
                    &entry.config.fallthrough,
                    &entry.config.off_variant,
                    enabled,
                )}
            </td>

            <td class="cell-secondary">{relative_time(&entry.config.updated_at)}</td>

            <td class="cell-actions">
                <Switch
                    checked=Signal::derive(move || enabled)
                    disabled=Signal::derive(move || !can_write.get())
                    label=toggle_label
                    on_change=Callback::new(move |_| on_toggle.run(for_toggle.clone()))
                />
            </td>
        </tr>
    }
}

/// One-line summary of what a flag is doing, so the list answers "what is going
/// on here" without opening every flag.
fn describe_targeting(
    rules: usize,
    fallthrough: &Distribution,
    off_variant: &str,
    enabled: bool,
) -> String {
    if !enabled {
        return "—".to_owned();
    }

    // Phrased to read as a sentence in both shapes: "everyone gets on" when
    // there is nothing else to say, and "1 rule, then on" when there is.
    let base = match fallthrough {
        Distribution::Fixed { variant } => {
            if rules == 0 {
                format!("everyone gets {variant}")
            } else {
                variant.clone()
            }
        }
        Distribution::Rollout { weights, .. } => {
            // Report the share that is *receiving* the feature. Summarising a
            // 25% rollout as "75% off" is true and useless: the number an
            // operator is watching is how far the rollout has gone.
            let headline = weights
                .iter()
                .filter(|w| w.variant != off_variant)
                .max_by_key(|w| w.weight)
                .or_else(|| weights.iter().max_by_key(|w| w.weight));

            match headline {
                Some(entry) => {
                    let percent = f64::from(entry.weight) / f64::from(TOTAL_WEIGHT) * 100.0;
                    format!("{}% {}", trim_zeros(percent), entry.variant)
                }
                None => "rollout".to_owned(),
            }
        }
    };

    match rules {
        0 => base,
        1 => format!("1 rule, then {base}"),
        n => format!("{n} rules, then {base}"),
    }
}

/// `25` rather than `25.0000`, `0.5` rather than `0.5000`.
fn trim_zeros(percent: f64) -> String {
    let rendered = format!("{percent:.4}");
    rendered.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[component]
fn CreateFlag(
    project: Signal<String>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_created: Callback<String>,
) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let name = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    let key_is_derived = RwSignal::new(true);
    let busy = RwSignal::new(false);

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);

        let Some(token) = session.token_untracked() else { return };
        let (project, name_value, key_value) =
            (project.get_untracked(), name.get_untracked(), key.get_untracked());

        leptos::task::spawn_local(async move {
            let result = api::create_flag(
                &token,
                &project,
                NewFlag {
                    key: &key_value,
                    name: &name_value,
                    description: None,
                    variants: None,
                    off_variant: None,
                    fallthrough: None,
                },
            )
            .await;

            busy.set(false);
            match result {
                Ok(flag) => on_created.run(flag.key),
                Err(error) => toaster.error("Could not create the flag", &error),
            }
        });
    };

    view! {
        <Modal title="New flag" on_close=on_close>
            <form on:submit=submit>
                <div class="card__body stack">
                    <div class="field">
                        <label class="label" for="flag-name">
                            "Name"
                        </label>
                        <input
                            id="flag-name"
                            class="input"
                            required=true
                            placeholder="New checkout"
                            prop:value=move || name.get()
                            on:input=move |e| {
                                let value = event_target_value(&e);
                                if key_is_derived.get_untracked() {
                                    key.set(super::projects_slug(&value));
                                }
                                name.set(value);
                            }
                        />
                    </div>

                    <div class="field">
                        <label class="label" for="flag-key">
                            "Key"
                        </label>
                        <input
                            id="flag-key"
                            class="input input--mono"
                            required=true
                            placeholder="checkout.v2"
                            prop:value=move || key.get()
                            on:input=move |e| {
                                key_is_derived.set(false);
                                key.set(event_target_value(&e));
                            }
                        />
                        <span class="hint">"What your code asks for. This cannot change later."</span>
                    </div>

                    <div class="callout">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "Boolean on/off flag, created disabled in every environment. Nothing changes until you turn it on."
                        </span>
                    </div>
                </div>

                <div class="card__footer">
                    <div class="spacer"></div>
                    <button
                        class="btn btn--ghost"
                        type="button"
                        on:click=move |_| on_close.run(())
                    >
                        "Cancel"
                    </button>
                    <button class="btn btn--primary" type="submit" disabled=move || busy.get()>
                        <Show when=move || busy.get()>
                            <span class="spinner"></span>
                        </Show>
                        "Create flag"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

#[component]
fn CreateEnvironment(
    project: Signal<String>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_created: Callback<String>,
) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let name = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    let key_is_derived = RwSignal::new(true);
    let is_production = RwSignal::new(false);
    let busy = RwSignal::new(false);

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);

        let Some(token) = session.token_untracked() else { return };
        let (project, name_value, key_value, production) = (
            project.get_untracked(),
            name.get_untracked(),
            key.get_untracked(),
            is_production.get_untracked(),
        );

        leptos::task::spawn_local(async move {
            let result = api::create_environment(
                &token,
                &project,
                NewEnvironment { key: &key_value, name: &name_value, is_production: production },
            )
            .await;

            busy.set(false);
            match result {
                Ok(environment) => on_created.run(environment.key),
                Err(error) => toaster.error("Could not create the environment", &error),
            }
        });
    };

    view! {
        <Modal title="New environment" on_close=on_close>
            <form on:submit=submit>
                <div class="card__body stack">
                    <div class="field">
                        <label class="label" for="env-name">
                            "Name"
                        </label>
                        <input
                            id="env-name"
                            class="input"
                            required=true
                            placeholder="Staging"
                            prop:value=move || name.get()
                            on:input=move |e| {
                                let value = event_target_value(&e);
                                if key_is_derived.get_untracked() {
                                    key.set(super::projects_slug(&value));
                                }
                                name.set(value);
                            }
                        />
                    </div>

                    <div class="field">
                        <label class="label" for="env-key">
                            "Key"
                        </label>
                        <input
                            id="env-key"
                            class="input input--mono"
                            required=true
                            placeholder="staging"
                            prop:value=move || key.get()
                            on:input=move |e| {
                                key_is_derived.set(false);
                                key.set(event_target_value(&e));
                            }
                        />
                    </div>

                    <label class="row" style="gap:var(--space-3);cursor:pointer">
                        <Switch
                            checked=Signal::derive(move || is_production.get())
                            label="Mark as production"
                            on_change=Callback::new(move |value| is_production.set(value))
                        />
                        <span>
                            <span style="font-weight:540">"Production environment"</span>
                            <span class="hint" style="display:block">
                                "Shown first and flagged in the UI."
                            </span>
                        </span>
                    </label>

                    <div class="callout">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "Gets its own bucketing salt, so a rollout here selects a different sample of users than the same rollout elsewhere."
                        </span>
                    </div>
                </div>

                <div class="card__footer">
                    <div class="spacer"></div>
                    <button
                        class="btn btn--ghost"
                        type="button"
                        on:click=move |_| on_close.run(())
                    >
                        "Cancel"
                    </button>
                    <button class="btn btn--primary" type="submit" disabled=move || busy.get()>
                        <Show when=move || busy.get()>
                            <span class="spinner"></span>
                        </Show>
                        "Create environment"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
