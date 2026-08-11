//! A/B experiments for one environment.
//!
//! Organised like the segments page — pick an environment, then work inside
//! it — because an experiment *is* a measurement of one environment's traffic.
//! The list shows lifecycle at a glance; opening an experiment shows its
//! judged results: rates, intervals and the verdict against the control.

use flagforge_core::VariantResult;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{
    self, Load, NewExperiment,
    models::{ConfiguredFlag, Environment, Experiment, ExperimentResults},
};
use crate::app::{PageHeader, fixed};
use crate::components::{ConfirmButton, Empty, Failure, Icon, Modal, SkeletonRows, defer};
use crate::pages::projects_slug;
use crate::session::use_session;
use crate::toast::use_toaster;

#[component]
pub fn Experiments() -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();
    let params = use_params_map();

    let project_key = move || params.read().get("project").unwrap_or_default();

    let environments = RwSignal::new(Load::<Vec<Environment>>::Loading);
    let selected_env = RwSignal::new(Option::<String>::None);
    let experiments = RwSignal::new(Load::<Vec<Experiment>>::Loading);
    let opened = RwSignal::new(Option::<Load<ExperimentResults>>::None);
    let opened_key = RwSignal::new(Option::<String>::None);
    let creating = RwSignal::new(false);

    let load_environments = move || {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        leptos::task::spawn_local(async move {
            let list = api::list_environments(&token, &project).await;
            if let Ok(list) = &list {
                selected_env.set(
                    list.iter()
                        .find(|e| e.is_production)
                        .or_else(|| list.first())
                        .map(|e| e.key.clone()),
                );
            }
            environments.set(list.into());
        });
    };

    let load_experiments = move || {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        experiments.set(Load::Loading);
        leptos::task::spawn_local(async move {
            experiments.set(api::list_experiments(&token, &project, &environment).await.into());
        });
    };

    let open = move |key: String| {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        opened_key.set(Some(key.clone()));
        opened.set(Some(Load::Loading));
        leptos::task::spawn_local(async move {
            opened.set(Some(
                api::experiment_results(&token, &project, &environment, &key).await.into(),
            ));
        });
    };

    Effect::new(move |_| {
        let _ = project_key();
        load_environments();
    });

    Effect::new(move |_| {
        let _ = selected_env.get();
        opened.set(None);
        opened_key.set(None);
        load_experiments();
    });

    let can_write = move || session.identity().is_some_and(|me| me.user.can_write());

    // Start and stop share everything but the verb and the toast.
    let transition = move |key: String, action: &'static str| {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        leptos::task::spawn_local(async move {
            match api::transition_experiment(&token, &project, &environment, &key, action).await {
                Ok(_) => {
                    toaster.success(if action == "start" {
                        "Experiment running — SDKs will start recording"
                    } else {
                        "Experiment stopped — the results are final"
                    });
                    load_experiments();
                    if opened_key.get_untracked().as_deref() == Some(key.as_str()) {
                        open(key);
                    }
                }
                Err(error) => toaster.error("Could not change the experiment", &error),
            }
        });
    };

    let delete = move |key: String| {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        leptos::task::spawn_local(async move {
            match api::delete_experiment(&token, &project, &environment, &key).await {
                Ok(()) => {
                    toaster.success("Experiment deleted");
                    opened.set(None);
                    opened_key.set(None);
                    load_experiments();
                }
                Err(error) => toaster.error("Could not delete the experiment", &error),
            }
        });
    };

    view! {
        <PageHeader
            title=fixed("Experiments")
            lead=fixed(
                "A flag measured against a metric: exposures, conversions, and whether the difference is real.",
            )
        >
            <A href=move || format!("/projects/{}", project_key()) attr:class="btn btn--ghost">
                <Icon name="back" />
                "Back"
            </A>
            <Show when=can_write>
                <button class="btn btn--primary" on:click=move |_| creating.set(true)>
                    <Icon name="plus" />
                    "New experiment"
                </button>
            </Show>
        </PageHeader>

        <div class="content stack">
            {move || match environments.get() {
                Load::Ready(list) => {
                    view! {
                        <div class="tabs" role="tablist">
                            {list
                                .into_iter()
                                .map(|environment| {
                                    let key = environment.key.clone();
                                    let active = {
                                        let key = key.clone();
                                        Signal::derive(move || {
                                            selected_env.get().as_deref() == Some(key.as_str())
                                        })
                                    };
                                    view! {
                                        <button
                                            class="tab"
                                            role="tab"
                                            type="button"
                                            aria-selected=move || active.get().to_string()
                                            on:click=move |_| selected_env.set(Some(key.clone()))
                                        >
                                            {environment.name.clone()}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
                _ => view! { <div class="skeleton" style="height:34px;width:220px"></div> }.into_any(),
            }}

            {move || match experiments.get() {
                Load::Loading => {
                    view! { <div class="card"><SkeletonRows rows=2 /></div> }.into_any()
                }
                Load::Failed(error) => {
                    view! {
                        <div class="card">
                            <Failure
                                error=error
                                on_retry=Callback::new(move |_| load_experiments())
                            />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) if list.is_empty() => {
                    view! {
                        <div class="card">
                            <Empty
                                icon="flask"
                                title="No experiments in this environment"
                                text="An experiment binds a flag to a conversion metric — its variants become the arms, and the traffic you already serve becomes the sample."
                            />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) => {
                    view! {
                        <div class="card">
                            <div class="table__scroll">
                                <table class="table">
                                    <thead>
                                        <tr>
                                            <th>"Key"</th>
                                            <th>"Measures"</th>
                                            <th>"State"</th>
                                            <th style="text-align:right"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {list
                                            .into_iter()
                                            .map(|experiment| {
                                                let key = experiment.key.clone();
                                                let open_key = key.clone();
                                                let start_key = key.clone();
                                                let stop_key = key.clone();
                                                let state = experiment.state.clone();
                                                let is_draft = state == "draft";
                                                let is_running = state == "running";
                                                // Callbacks are Copy, so the Show children stay Fn.
                                                let start_now = Callback::new(move |_: ()| {
                                                    transition(start_key.clone(), "start")
                                                });
                                                let stop_now = Callback::new(move |_: ()| {
                                                    transition(stop_key.clone(), "stop")
                                                });
                                                view! {
                                                    <tr>
                                                        <td>
                                                            <code class="cell-key">{experiment.key.clone()}</code>
                                                            <div class="cell-secondary">{experiment.name.clone()}</div>
                                                        </td>
                                                        <td class="cell-secondary">
                                                            <code class="cell-key">{experiment.flag_key.clone()}</code>
                                                            " → "
                                                            <code class="cell-key">{experiment.metric_key.clone()}</code>
                                                        </td>
                                                        <td><StateBadge state=state.clone() /></td>
                                                        <td class="cell-actions">
                                                            <Show when=move || can_write() && is_draft>
                                                                <button
                                                                    class="btn btn--secondary btn--sm"
                                                                    on:click=move |_| start_now.run(())
                                                                >
                                                                    "Start"
                                                                </button>
                                                            </Show>
                                                            <Show when=move || can_write() && is_running>
                                                                <button
                                                                    class="btn btn--ghost btn--sm"
                                                                    on:click=move |_| stop_now.run(())
                                                                >
                                                                    "Stop"
                                                                </button>
                                                            </Show>
                                                            <button
                                                                class="btn btn--ghost btn--sm"
                                                                on:click=move |_| open(open_key.clone())
                                                            >
                                                                "Results"
                                                            </button>
                                                        </td>
                                                    </tr>
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

            {move || {
                opened
                    .get()
                    .map(|load| match load {
                        Load::Loading => {
                            view! { <div class="card"><SkeletonRows rows=3 /></div> }.into_any()
                        }
                        Load::Failed(error) => {
                            view! {
                                <div class="card">
                                    <Failure
                                        error=error
                                        on_retry=Callback::new(move |_| {
                                            if let Some(key) = opened_key.get_untracked() {
                                                open(key);
                                            }
                                        })
                                    />
                                </div>
                            }
                                .into_any()
                        }
                        Load::Ready(loaded) => {
                            let key = loaded.experiment.key.clone();
                            let delete_this = Callback::new(move |_| delete(key.clone()));
                            view! {
                                <ResultsPanel
                                    loaded=loaded
                                    can_write=Signal::derive(can_write)
                                    on_close=Callback::new(move |_| {
                                        opened.set(None);
                                        opened_key.set(None);
                                    })
                                    on_delete=delete_this
                                />
                            }
                                .into_any()
                        }
                    })
            }}
        </div>

        <Show when=move || creating.get()>
            <CreateExperiment
                project=Signal::derive(project_key)
                environment=Signal::derive(move || selected_env.get().unwrap_or_default())
                on_close=Callback::new(move |_| defer(move || creating.set(false)))
                on_created=Callback::new(move |key: String| {
                    creating.set(false);
                    load_experiments();
                    open(key);
                })
            />
        </Show>
    }
}

#[component]
fn StateBadge(state: String) -> impl IntoView {
    // The dot plus the word: state is never colour alone.
    match state.as_str() {
        "running" => view! {
            <span class="badge badge--on">
                <span class="dot"></span>
                "Running"
            </span>
        }
        .into_any(),
        "stopped" => view! { <span class="badge badge--off">"Stopped"</span> }.into_any(),
        _ => view! { <span class="badge">"Draft"</span> }.into_any(),
    }
}

/// The results view: one meter per arm on a shared scale, numbers beside it,
/// and the z-test's verdict in words.
#[component]
fn ResultsPanel(
    loaded: ExperimentResults,
    can_write: Signal<bool>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_delete: Callback<()>,
) -> impl IntoView {
    let experiment = loaded.experiment;
    let running = experiment.state == "running";
    let control = experiment.control_variant.clone();

    // One scale for every arm, snapped up to the next 5 % so the bars have
    // headroom and the axis note reads as a round number.
    let top = loaded
        .results
        .iter()
        .filter_map(|r| r.interval.map(|ci| ci.high).or(r.rate))
        .fold(0.0_f64, f64::max);
    let scale = ((top / 0.05).ceil() * 0.05).max(0.05);

    let no_data = loaded.results.iter().all(|r| r.exposures == 0 && r.conversions == 0);
    let has_excess = loaded.results.iter().any(|r| r.exposures > 0 && r.conversions > r.exposures);

    // Colour is keyed to the arm's position in the flag definition, matching
    // the distribution bar on the flag page, so an arm keeps its colour
    // between the two views.
    let position_of = {
        let variants: Vec<String> = experiment.variants.iter().map(|v| v.key.clone()).collect();
        move |variant: &str| variants.iter().position(|v| v == variant).unwrap_or(0) % 4
    };

    view! {
        <div class="card">
            <div class="card__header">
                <h2 class="card__title">
                    {experiment.name.clone()}
                    " "
                    <StateBadge state=experiment.state.clone() />
                </h2>
                <div class="spacer"></div>
                <button class="btn btn--ghost btn--sm" on:click=move |_| on_close.run(())>
                    "Close"
                </button>
            </div>

            <div class="card__body stack">
                <div class="kv">
                    <span class="cell-secondary">"Flag"</span>
                    <code class="cell-key">{experiment.flag_key.clone()}</code>
                    <span class="cell-secondary">"Metric"</span>
                    <code class="cell-key">{experiment.metric_key.clone()}</code>
                    <span class="cell-secondary">"Control"</span>
                    <code class="cell-key">{control.clone()}</code>
                    {experiment
                        .started_at
                        .clone()
                        .map(|at| {
                            view! {
                                <span class="cell-secondary">"Started"</span>
                                <span>{at.chars().take(10).collect::<String>()}</span>
                            }
                        })}
                    {experiment
                        .stopped_at
                        .clone()
                        .map(|at| {
                            view! {
                                <span class="cell-secondary">"Stopped"</span>
                                <span>{at.chars().take(10).collect::<String>()}</span>
                            }
                        })}
                </div>

                <Show when=move || no_data>
                    <div class="callout">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "No events yet. Exposures arrive when SDKs evaluate the flag; conversions when they call "
                            <code class="cell-key">"track(\"" {experiment.metric_key.clone()} "\")"</code>
                            "."
                        </span>
                    </div>
                </Show>

                <Show when=move || has_excess>
                    <div class="callout callout--accent">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "An arm has more conversions than exposures — the metric is being tracked for contexts that never evaluated the flag. Rates are clamped, raw counts shown."
                        </span>
                    </div>
                </Show>

                <div class="table__scroll">
                    <table class="table">
                        <thead>
                            <tr>
                                <th>"Arm"</th>
                                <th style="text-align:right;width:110px">"Exposures"</th>
                                <th style="text-align:right;width:110px">"Conversions"</th>
                                <th style="min-width:220px">
                                    "Conversion rate"
                                    <span class="cell-secondary" style="font-weight:400">
                                        {format!(" (scale 0–{:.0} %)", scale * 100.0)}
                                    </span>
                                </th>
                                <th>"Against control"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {loaded
                                .results
                                .iter()
                                .map(|arm| {
                                    let is_control = arm.variant == control;
                                    let position = position_of(&arm.variant);
                                    view! {
                                        <ArmRow
                                            arm=arm.clone()
                                            is_control=is_control
                                            position=position
                                            scale=scale
                                        />
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                </div>
            </div>

            <div class="card__footer">
                <Show when=move || can_write.get()>
                    <Show
                        when=move || !running
                        fallback=|| {
                            view! {
                                <span class="cell-secondary">
                                    "Running experiments cannot be deleted — stop it first."
                                </span>
                            }
                        }
                    >
                        <ConfirmButton
                            label="Delete"
                            confirm_label="Confirm delete"
                            on_confirm=on_delete
                        />
                    </Show>
                </Show>
                <div class="spacer"></div>
            </div>
        </div>
    }
}

#[component]
fn ArmRow(arm: VariantResult, is_control: bool, position: usize, scale: f64) -> impl IntoView {
    let rate_text = arm.rate.map(|rate| format!("{:.1} %", rate * 100.0));
    let ci_text =
        arm.interval.map(|ci| format!("95 % CI {:.1}–{:.1} %", ci.low * 100.0, ci.high * 100.0));
    let meter_label = match (&rate_text, &ci_text) {
        (Some(rate), Some(ci)) => format!("{}: {rate}, {ci}", arm.variant),
        _ => format!("{}: no exposures yet", arm.variant),
    };

    let fill_width = arm.rate.map(|rate| format!("width:{:.2}%", (rate / scale) * 100.0));
    let ci_style = arm.interval.map(|ci| {
        format!(
            "left:{:.2}%;width:{:.2}%",
            (ci.low / scale) * 100.0,
            ((ci.high - ci.low) / scale).max(0.0) * 100.0,
        )
    });

    view! {
        <tr>
            <td>
                <code class="cell-key">{arm.variant.clone()}</code>
                <Show when=move || is_control>
                    <span class="badge" style="margin-left:6px">"control"</span>
                </Show>
            </td>
            <td style="text-align:right;font-variant-numeric:tabular-nums">{arm.exposures}</td>
            <td style="text-align:right;font-variant-numeric:tabular-nums">{arm.conversions}</td>
            <td>
                <div class="meter" role="img" aria-label=meter_label>
                    {fill_width
                        .map(|style| {
                            view! {
                                <div
                                    class=format!("meter__fill meter__fill--{position}")
                                    style=style
                                ></div>
                            }
                        })}
                    {ci_style
                        .map(|style| view! { <div class="meter__ci" style=style></div> })}
                </div>
                <div class="cell-secondary" style="margin-top:2px">
                    {match (rate_text, ci_text) {
                        (Some(rate), Some(ci)) => format!("{rate} · {ci}"),
                        _ => "No exposures yet".to_owned(),
                    }}
                </div>
            </td>
            <td>{verdict(&arm, is_control)}</td>
        </tr>
    }
}

/// The comparison cell, in words. The badge never carries the verdict alone.
fn verdict(arm: &VariantResult, is_control: bool) -> impl IntoView + use<> {
    if is_control {
        return view! { <span class="cell-secondary">"— baseline"</span> }.into_any();
    }
    match arm.vs_control {
        Some(comparison) => {
            let lift = format!("{:+.1} pts", comparison.lift * 100.0);
            let p = if comparison.p_value < 0.001 {
                "p < 0.001".to_owned()
            } else {
                format!("p = {:.3}", comparison.p_value)
            };
            let badge = if comparison.significant {
                view! {
                    <span class="badge badge--on">
                        <span class="dot"></span>
                        "Significant"
                    </span>
                }
                .into_any()
            } else {
                view! { <span class="badge">"Not significant"</span> }.into_any()
            };
            view! {
                <div class="stack" style="gap:4px">
                    {badge}
                    <span class="cell-secondary" style="font-variant-numeric:tabular-nums">
                        {format!("{lift} · {p}")}
                    </span>
                </div>
            }
            .into_any()
        }
        None => view! { <span class="cell-secondary">"Awaiting data"</span> }.into_any(),
    }
}

#[component]
fn CreateExperiment(
    project: Signal<String>,
    environment: Signal<String>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_created: Callback<String>,
) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let flags = RwSignal::new(Load::<Vec<ConfiguredFlag>>::Loading);
    let name = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    let key_touched = RwSignal::new(false);
    let flag_key = RwSignal::new(String::new());
    let metric_key = RwSignal::new(String::new());
    let control = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    // The flag picker needs the environment's flags; their variants feed the
    // control picker, and the off variant is the natural default control —
    // it is what traffic outside the experiment would have seen.
    Effect::new(move |_| {
        let (Some(token), project, environment) =
            (session.token_untracked(), project.get(), environment.get())
        else {
            return;
        };
        leptos::task::spawn_local(async move {
            let list = api::list_configured(&token, &project, &environment).await;
            if let Ok(list) = &list
                && let Some(first) = list.iter().find(|c| !c.flag.archived)
            {
                flag_key.set(first.flag.key.clone());
                control.set(first.config.off_variant.clone());
            }
            flags.set(list.into());
        });
    });

    let variants_of_selected = Signal::derive(move || match flags.get() {
        Load::Ready(list) => list
            .iter()
            .find(|c| c.flag.key == flag_key.get())
            .map(|c| c.flag.variants.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    });

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);

        let Some(token) = session.token_untracked() else { return };
        let (project, environment) = (project.get_untracked(), environment.get_untracked());
        let (name_value, key_value, flag_value, metric_value, control_value) = (
            name.get_untracked(),
            key.get_untracked(),
            flag_key.get_untracked(),
            metric_key.get_untracked(),
            control.get_untracked(),
        );

        leptos::task::spawn_local(async move {
            let result = api::create_experiment(
                &token,
                &project,
                &environment,
                NewExperiment {
                    key: &key_value,
                    name: &name_value,
                    description: None,
                    flag_key: &flag_value,
                    metric_key: &metric_value,
                    control_variant: &control_value,
                },
            )
            .await;

            busy.set(false);
            match result {
                Ok(created) => on_created.run(created.key),
                Err(error) => toaster.error("Could not create the experiment", &error),
            }
        });
    };

    view! {
        <Modal title="New experiment" on_close=on_close>
            <form on:submit=submit>
                <div class="card__body stack">
                    <div class="field">
                        <label class="label" for="new-experiment-name">
                            "Name"
                        </label>
                        <input
                            id="new-experiment-name"
                            class="input"
                            required=true
                            placeholder="Checkout call-to-action"
                            prop:value=move || name.get()
                            on:input=move |e| {
                                let value = event_target_value(&e);
                                if !key_touched.get_untracked() {
                                    key.set(projects_slug(&value));
                                }
                                name.set(value);
                            }
                        />
                    </div>

                    <div class="field">
                        <label class="label" for="new-experiment-key">
                            "Key"
                        </label>
                        <input
                            id="new-experiment-key"
                            class="input input--mono"
                            required=true
                            placeholder="checkout-cta"
                            prop:value=move || key.get()
                            on:input=move |e| {
                                key_touched.set(true);
                                key.set(event_target_value(&e));
                            }
                        />
                    </div>

                    <div class="field">
                        <label class="label" for="new-experiment-flag">
                            "Flag"
                        </label>
                        <select
                            id="new-experiment-flag"
                            class="select"
                            on:change=move |e| {
                                let value = event_target_value(&e);
                                if let Load::Ready(list) = flags.get_untracked()
                                    && let Some(chosen) = list.iter().find(|c| c.flag.key == value)
                                {
                                    control.set(chosen.config.off_variant.clone());
                                }
                                flag_key.set(value);
                            }
                        >
                            {move || match flags.get() {
                                Load::Ready(list) => list
                                    .into_iter()
                                    .filter(|c| !c.flag.archived)
                                    .map(|c| {
                                        let selected = c.flag.key == flag_key.get_untracked();
                                        view! {
                                            <option value=c.flag.key.clone() selected=selected>
                                                {c.flag.key.clone()}
                                            </option>
                                        }
                                    })
                                    .collect_view()
                                    .into_any(),
                                _ => view! { <option>"Loading…"</option> }.into_any(),
                            }}
                        </select>
                        <span class="hint">
                            "Its variants become the arms. Fixed once created."
                        </span>
                    </div>

                    <div class="field">
                        <label class="label" for="new-experiment-metric">
                            "Conversion metric"
                        </label>
                        <input
                            id="new-experiment-metric"
                            class="input input--mono"
                            required=true
                            placeholder="order.completed"
                            prop:value=move || metric_key.get()
                            on:input=move |e| metric_key.set(event_target_value(&e))
                        />
                        <span class="hint">
                            "What your service passes to " <code class="cell-key">"track()"</code>
                            " when the thing you care about happens."
                        </span>
                    </div>

                    <div class="field">
                        <label class="label" for="new-experiment-control">
                            "Control variant"
                        </label>
                        <select
                            id="new-experiment-control"
                            class="select"
                            on:change=move |e| control.set(event_target_value(&e))
                        >
                            {move || {
                                variants_of_selected
                                    .get()
                                    .into_iter()
                                    .map(|variant| {
                                        let selected = variant.key == control.get_untracked();
                                        view! {
                                            <option value=variant.key.clone() selected=selected>
                                                {variant.key.clone()}
                                            </option>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </select>
                        <span class="hint">
                            "The baseline every other arm is judged against — usually the behaviour users had before."
                        </span>
                    </div>

                    <div class="callout">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "Created as a draft. Nothing is measured until you press Start, and a stopped experiment cannot restart."
                        </span>
                    </div>
                </div>

                <div class="card__footer">
                    <div class="spacer"></div>
                    <button class="btn btn--ghost" type="button" on:click=move |_| on_close.run(())>
                        "Cancel"
                    </button>
                    <button class="btn btn--primary" type="submit" disabled=move || busy.get()>
                        <Show when=move || busy.get()>
                            <span class="spinner"></span>
                        </Show>
                        "Create experiment"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
