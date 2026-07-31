//! Flag detail: the per-environment configuration editor.
//!
//! The editor works against a local draft and validates it with
//! `flagforge_core::validate` — the same function the server runs — so an
//! invalid rollout is caught before a request is made rather than coming back
//! as a 422.

use flagforge_core::{
    AttributeValue, Condition, Distribution, EvaluationContext, Operator, Reason, Rule,
    TOTAL_WEIGHT, ValidationIssue, Variant, WeightedVariant,
};
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{self, ConfigBody, Load, models::Environment};
use crate::app::PageHeader;
use crate::components::{ConfirmButton, Failure, Icon, SkeletonRows, Switch};
use crate::session::use_session;
use crate::toast::use_toaster;

/// Sample size for the distribution simulation.
///
/// Big enough that the numbers are stable, small enough to stay well under a
/// frame — the whole point is that it feels instant.
const SIMULATED_SUBJECTS: u32 = 2_000;

/// Bucketing uses a per-environment salt that never leaves the server, so the
/// preview cannot reproduce a specific user's assignment. It uses a fixed
/// stand-in and says so; the *aggregate* split it reports is salt-independent
/// and therefore exact.
const PREVIEW_SALT: &str = "preview";

#[derive(Clone, Debug, PartialEq)]
struct Draft {
    enabled: bool,
    off_variant: String,
    fallthrough: Distribution,
    rules: Vec<Rule>,
    version: i64,
}

#[component]
pub fn FlagDetail() -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();
    let params = use_params_map();

    let project_key = move || params.read().get("project").unwrap_or_default();
    let flag_key = move || params.read().get("flag").unwrap_or_default();

    let environments = RwSignal::new(Load::<Vec<Environment>>::Loading);
    let selected = RwSignal::new(Option::<String>::None);
    let variants = RwSignal::new(Vec::<Variant>::new());
    let loaded = RwSignal::new(Load::<Draft>::Loading);
    let draft = RwSignal::new(Option::<Draft>::None);
    let saving = RwSignal::new(false);

    let load_environments = move || {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        leptos::task::spawn_local(async move {
            let list = api::list_environments(&token, &project).await;
            if let Ok(list) = &list {
                selected.set(
                    list.iter()
                        .find(|e| e.is_production)
                        .or_else(|| list.first())
                        .map(|e| e.key.clone()),
                );
            }
            environments.set(list.into());
        });
    };

    let load_config = move || {
        let (Some(token), project, flag) = (session.token_untracked(), project_key(), flag_key())
        else {
            return;
        };
        let Some(environment) = selected.get_untracked() else { return };

        loaded.set(Load::Loading);
        leptos::task::spawn_local(async move {
            let definition = api::get_flag(&token, &project, &flag).await;
            let config = api::get_config(&token, &project, &environment, &flag).await;

            match (definition, config) {
                (Ok(definition), Ok(config)) => {
                    variants.set(definition.variants);
                    let value = Draft {
                        enabled: config.enabled,
                        off_variant: config.off_variant,
                        fallthrough: config.fallthrough,
                        rules: config.rules,
                        version: config.version,
                    };
                    draft.set(Some(value.clone()));
                    loaded.set(Load::Ready(value));
                }
                (Err(error), _) | (_, Err(error)) => loaded.set(Load::Failed(error)),
            }
        });
    };

    Effect::new(move |_| {
        let _ = (project_key(), flag_key());
        load_environments();
    });

    Effect::new(move |_| {
        let _ = selected.get();
        load_config();
    });

    let can_write = move || session.identity().is_some_and(|me| me.user.can_write());

    // Live validation against the domain validator the server uses.
    let issues = Memo::new(move |_| match (draft.get(), variants.get()) {
        (Some(draft), variants) if !variants.is_empty() => {
            let candidate = flagforge_core::Flag {
                key: flag_key(),
                variants,
                enabled: true,
                off_variant: draft.off_variant,
                fallthrough: draft.fallthrough,
                rules: draft.rules,
                version: draft.version,
            };
            flagforge_core::validate(&candidate).err().unwrap_or_default()
        }
        _ => Vec::new(),
    });

    let dirty = Memo::new(move |_| match (draft.get(), loaded.get()) {
        (Some(current), Load::Ready(original)) => current != original,
        _ => false,
    });

    let save = move |_| {
        let (Some(token), project, flag) = (session.token_untracked(), project_key(), flag_key())
        else {
            return;
        };
        let (Some(environment), Some(current)) = (selected.get_untracked(), draft.get_untracked())
        else {
            return;
        };
        if saving.get_untracked() {
            return;
        }
        saving.set(true);

        leptos::task::spawn_local(async move {
            let result = api::put_config(
                &token,
                &project,
                &environment,
                &flag,
                ConfigBody {
                    enabled: current.enabled,
                    off_variant: current.off_variant.clone(),
                    fallthrough: current.fallthrough.clone(),
                    rules: current.rules.clone(),
                    expected_version: current.version,
                },
            )
            .await;

            saving.set(false);
            match result {
                Ok(config) => {
                    let saved = Draft {
                        enabled: config.enabled,
                        off_variant: config.off_variant,
                        fallthrough: config.fallthrough,
                        rules: config.rules,
                        version: config.version,
                    };
                    draft.set(Some(saved.clone()));
                    loaded.set(Load::Ready(saved));
                    toaster.success(format!("Saved — {flag} updated in {environment}"));
                }
                Err(error) if error.is_conflict() => {
                    toaster.message(
                        crate::toast::Level::Error,
                        "Someone else saved first",
                        "Your changes were not applied. Reload to see theirs.",
                    );
                }
                Err(error) => toaster.error("Could not save", &error),
            }
        });
    };

    view! {
        <PageHeader
            title=Signal::derive(flag_key)
            lead=Signal::derive(move || {
                format!("Configuration in {}", selected.get().unwrap_or_default())
            })
        >
            <A
                href=move || format!("/projects/{}", project_key())
                attr:class="btn btn--ghost"
            >
                <Icon name="back" />
                "Back"
            </A>
            <Show when=move || can_write() && dirty.get()>
                <button
                    class="btn btn--primary"
                    disabled=move || saving.get() || !issues.get().is_empty()
                    on:click=save
                >
                    <Show when=move || saving.get()>
                        <span class="spinner"></span>
                    </Show>
                    "Save changes"
                </button>
            </Show>
        </PageHeader>

        <div class="content stack">
            <EnvironmentTabs environments=environments selected=selected />

            {move || match (loaded.get(), draft.get()) {
                (Load::Loading, _) | (_, None) => {
                    view! { <div class="card"><SkeletonRows rows=4 /></div> }.into_any()
                }
                (Load::Failed(error), _) => {
                    view! {
                        <div class="card">
                            <Failure error=error on_retry=Callback::new(move |_| load_config()) />
                        </div>
                    }
                        .into_any()
                }
                (Load::Ready(_), Some(_)) => {
                    view! {
                        <>
                            <Show when=move || !issues.get().is_empty()>
                                <IssueList issues=Signal::derive(move || issues.get()) />
                            </Show>

                            <MasterSwitch draft=draft can_write=Signal::derive(can_write) />

                            <RulesEditor
                                draft=draft
                                variants=Signal::derive(move || variants.get())
                                can_write=Signal::derive(can_write)
                            />

                            <FallthroughEditor
                                draft=draft
                                variants=Signal::derive(move || variants.get())
                                can_write=Signal::derive(can_write)
                            />

                            <Preview
                                draft=draft
                                variants=Signal::derive(move || variants.get())
                                flag_key=Signal::derive(flag_key)
                            />

                            <Show when=can_write>
                                <DangerZone
                                    project=Signal::derive(project_key)
                                    flag_key=Signal::derive(flag_key)
                                />
                            </Show>
                        </>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn EnvironmentTabs(
    environments: RwSignal<Load<Vec<Environment>>>,
    selected: RwSignal<Option<String>>,
) -> impl IntoView {
    view! {
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
                                    move || selected.get().as_deref() == Some(key.as_str())
                                };
                                let choose = move |_| selected.set(Some(key.clone()));
                                view! {
                                    <button
                                        class="tab"
                                        role="tab"
                                        type="button"
                                        aria-selected=move || active().to_string()
                                        on:click=choose
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
    }
}

#[component]
fn IssueList(issues: Signal<Vec<ValidationIssue>>) -> impl IntoView {
    view! {
        <div class="callout callout--danger" role="alert">
            <div style="width:16px;height:16px;flex:none">
                <Icon name="alert" />
            </div>
            <div>
                <strong>"This configuration cannot be saved yet"</strong>
                <ul style="margin:var(--space-2) 0 0;padding-left:var(--space-4)">
                    {move || {
                        issues
                            .get()
                            .into_iter()
                            .map(|issue| {
                                view! {
                                    <li>
                                        <code>{issue.path}</code>
                                        " — "
                                        {issue.message}
                                    </li>
                                }
                            })
                            .collect_view()
                    }}
                </ul>
            </div>
        </div>
    }
}

#[component]
fn MasterSwitch(draft: RwSignal<Option<Draft>>, can_write: Signal<bool>) -> impl IntoView {
    let enabled = Signal::derive(move || draft.get().is_some_and(|d| d.enabled));

    view! {
        <div class="card">
            <div class="card__body row" style="gap:var(--space-4)">
                <Switch
                    checked=enabled
                    disabled=Signal::derive(move || !can_write.get())
                    label="Enable this flag in this environment"
                    on_change=Callback::new(move |value: bool| {
                        draft.update(|d| {
                            if let Some(d) = d {
                                d.enabled = value;
                            }
                        })
                    })
                />
                <div>
                    <div style="font-weight:580">
                        {move || if enabled.get() { "Flag is on" } else { "Flag is off" }}
                    </div>
                    <div class="hint">
                        {move || {
                            if enabled.get() {
                                "Rules are evaluated in order, then the fallthrough below."
                            } else {
                                "Everyone gets the off variant. Rules are not evaluated at all."
                            }
                        }}
                    </div>
                </div>
            </div>
        </div>
    }
}

#[component]
fn RulesEditor(
    draft: RwSignal<Option<Draft>>,
    variants: Signal<Vec<Variant>>,
    can_write: Signal<bool>,
) -> impl IntoView {
    let rules = Signal::derive(move || draft.get().map(|d| d.rules).unwrap_or_default());

    let add_rule = move |_| {
        let default_variant = variants.get_untracked().first().map(|v| v.key.clone());
        let Some(variant) = default_variant else { return };

        draft.update(|d| {
            if let Some(d) = d {
                d.rules.push(Rule {
                    id: new_uuid(),
                    description: None,
                    conditions: vec![Condition::new(
                        "plan",
                        Operator::In,
                        vec![AttributeValue::String("pro".into())],
                    )],
                    distribution: Distribution::Fixed { variant },
                });
            }
        });
    };

    view! {
        <div class="card">
            <div class="card__header">
                <h2 class="card__title">"Targeting rules"</h2>
                <span class="hint">"Evaluated top to bottom; the first match wins."</span>
                <div class="spacer"></div>
                <Show when=move || can_write.get()>
                    <button class="btn btn--secondary btn--sm" on:click=add_rule>
                        <Icon name="plus" />
                        "Add rule"
                    </button>
                </Show>
            </div>

            <div class="card__body stack">
                <Show
                    when=move || !rules.get().is_empty()
                    fallback=|| {
                        view! {
                            <p class="hint">
                                "No rules — everyone falls through to the distribution below."
                            </p>
                        }
                    }
                >
                    {move || {
                        rules
                            .get()
                            .into_iter()
                            .enumerate()
                            .map(|(index, rule)| {
                                view! {
                                    <RuleCard
                                        index=index
                                        rule=rule
                                        draft=draft
                                        variants=variants
                                        can_write=can_write
                                    />
                                }
                            })
                            .collect_view()
                    }}
                </Show>
            </div>
        </div>
    }
}

#[component]
fn RuleCard(
    index: usize,
    rule: Rule,
    draft: RwSignal<Option<Draft>>,
    variants: Signal<Vec<Variant>>,
    can_write: Signal<bool>,
) -> impl IntoView {
    let remove = move |_| {
        draft.update(|d| {
            if let Some(d) = d {
                d.rules.remove(index);
            }
        })
    };

    let serve = rule.distribution.clone();
    let selected_variant = match &serve {
        Distribution::Fixed { variant } => variant.clone(),
        Distribution::Rollout { weights, .. } => {
            weights.first().map(|w| w.variant.clone()).unwrap_or_default()
        }
    };

    view! {
        <div class="rule">
            <div class="rule__header">
                <span class="rule__order">{index + 1}</span>
                <input
                    class="input"
                    style="border:0;background:transparent;padding:2px 4px;font-weight:540"
                    placeholder="Describe this rule…"
                    disabled=move || !can_write.get()
                    prop:value=rule.description.clone().unwrap_or_default()
                    on:change=move |e| {
                        let text = event_target_value(&e);
                        draft
                            .update(|d| {
                                if let Some(d) = d
                                    && let Some(rule) = d.rules.get_mut(index)
                                {
                                    rule
                                        .description = (!text.trim().is_empty())
                                        .then(|| text.trim().to_owned());
                                }
                            })
                    }
                />
                <div class="spacer"></div>
                <Show when=move || can_write.get()>
                    <button
                        class="btn btn--ghost btn--icon btn--sm"
                        aria-label="Remove rule"
                        on:click=remove
                    >
                        <Icon name="trash" />
                    </button>
                </Show>
            </div>

            <div class="rule__body">
                {rule
                    .conditions
                    .iter()
                    .enumerate()
                    .map(|(position, condition)| {
                        view! {
                            <ConditionRow
                                rule_index=index
                                position=position
                                condition=condition.clone()
                                draft=draft
                                can_write=can_write
                            />
                        }
                    })
                    .collect_view()}

                <Show when=move || can_write.get()>
                    <button
                        class="btn btn--ghost btn--sm"
                        style="align-self:flex-start"
                        on:click=move |_| {
                            draft
                                .update(|d| {
                                    if let Some(d) = d
                                        && let Some(rule) = d.rules.get_mut(index)
                                    {
                                        rule
                                            .conditions
                                            .push(
                                                Condition::new("country", Operator::In, vec![
                                                    AttributeValue::String("ES".into()),
                                                ]),
                                            );
                                    }
                                })
                        }
                    >
                        <Icon name="plus" />
                        "Add condition"
                    </button>
                </Show>

                <div class="row" style="gap:var(--space-3);padding-top:var(--space-2)">
                    <span class="condition__joiner">"Then serve"</span>
                    <select
                        class="select"
                        style="max-width:200px"
                        disabled=move || !can_write.get()
                        on:change=move |e| {
                            let variant = event_target_value(&e);
                            draft
                                .update(|d| {
                                    if let Some(d) = d
                                        && let Some(rule) = d.rules.get_mut(index)
                                    {
                                        rule.distribution = Distribution::Fixed { variant };
                                    }
                                })
                        }
                    >
                        {move || {
                            let current = selected_variant.clone();
                            variants
                                .get()
                                .into_iter()
                                .map(|variant| {
                                    let selected = variant.key == current;
                                    let (value, text) = (variant.key.clone(), variant.key);
                                    view! {
                                        <option value=value selected=selected>
                                            {text}
                                        </option>
                                    }
                                })
                                .collect_view()
                        }}
                    </select>
                </div>
            </div>
        </div>
    }
}

#[component]
fn ConditionRow(
    rule_index: usize,
    position: usize,
    condition: Condition,
    draft: RwSignal<Option<Draft>>,
    can_write: Signal<bool>,
) -> impl IntoView {
    let mutate = move |apply: Box<dyn Fn(&mut Condition)>| {
        draft.update(|d| {
            if let Some(d) = d
                && let Some(rule) = d.rules.get_mut(rule_index)
                && let Some(condition) = rule.conditions.get_mut(position)
            {
                apply(condition);
            }
        })
    };

    let current_operator = condition.operator;
    let values_text =
        condition.values.iter().filter_map(|v| v.to_text()).collect::<Vec<_>>().join(", ");

    view! {
        <div class="condition">
            <div class="row" style="gap:var(--space-2)">
                <span class="condition__joiner">
                    {if position == 0 { "If" } else { "And" }}
                </span>
                <input
                    class="input input--mono"
                    placeholder="attribute"
                    disabled=move || !can_write.get()
                    prop:value=condition.attribute.clone()
                    on:change=move |e| {
                        let value = event_target_value(&e);
                        mutate(Box::new(move |c| c.attribute = value.clone()));
                    }
                />
            </div>

            <select
                class="select"
                disabled=move || !can_write.get()
                on:change=move |e| {
                    let raw = event_target_value(&e);
                    mutate(
                        Box::new(move |c| {
                            if let Some(operator) = operator_from(&raw) {
                                c.operator = operator;
                            }
                        }),
                    );
                }
            >
                {OPERATORS
                    .iter()
                    .map(|(value, label)| {
                        view! {
                            <option
                                value=*value
                                selected=operator_key(current_operator) == *value
                            >
                                {*label}
                            </option>
                        }
                    })
                    .collect_view()}
            </select>

            <input
                class="input"
                placeholder="value, value, …"
                disabled=move || !can_write.get() || !current_operator.takes_values()
                prop:value=values_text
                on:change=move |e| {
                    let raw = event_target_value(&e);
                    mutate(Box::new(move |c| c.values = parse_values(&raw)));
                }
            />

            <Show when=move || can_write.get()>
                <button
                    class="btn btn--ghost btn--icon btn--sm"
                    aria-label="Remove condition"
                    on:click=move |_| {
                        draft
                            .update(|d| {
                                if let Some(d) = d
                                    && let Some(rule) = d.rules.get_mut(rule_index)
                                {
                                    rule.conditions.remove(position);
                                }
                            })
                    }
                >
                    <Icon name="close" />
                </button>
            </Show>
        </div>
    }
}

#[component]
fn FallthroughEditor(
    draft: RwSignal<Option<Draft>>,
    variants: Signal<Vec<Variant>>,
    can_write: Signal<bool>,
) -> impl IntoView {
    let fallthrough = Signal::derive(move || {
        draft.get().map(|d| d.fallthrough).unwrap_or(Distribution::Fixed { variant: String::new() })
    });

    let is_rollout =
        Signal::derive(move || matches!(fallthrough.get(), Distribution::Rollout { .. }));

    let set_mode = move |rollout: bool| {
        let list = variants.get_untracked();
        draft.update(|d| {
            let Some(d) = d else { return };
            d.fallthrough = if rollout {
                Distribution::Rollout { weights: even_split(&list), bucket_by: None }
            } else {
                Distribution::Fixed {
                    variant: list.first().map(|v| v.key.clone()).unwrap_or_default(),
                }
            };
        });
    };

    view! {
        <div class="card">
            <div class="card__header">
                <h2 class="card__title">"Default distribution"</h2>
                <span class="hint">"Applies to everyone no rule matched."</span>
            </div>

            <div class="card__body stack">
                <div class="row" style="gap:var(--space-1)">
                    <button
                        class="btn btn--sm"
                        class:btn--primary=move || !is_rollout.get()
                        class:btn--secondary=move || is_rollout.get()
                        disabled=move || !can_write.get()
                        on:click=move |_| set_mode(false)
                    >
                        "Single variant"
                    </button>
                    <button
                        class="btn btn--sm"
                        class:btn--primary=is_rollout
                        class:btn--secondary=move || !is_rollout.get()
                        disabled=move || !can_write.get()
                        on:click=move |_| set_mode(true)
                    >
                        "Percentage rollout"
                    </button>
                </div>

                {move || match fallthrough.get() {
                    Distribution::Fixed { variant } => {
                        view! {
                            <select
                                class="select"
                                style="max-width:240px"
                                disabled=move || !can_write.get()
                                on:change=move |e| {
                                    let value = event_target_value(&e);
                                    draft
                                        .update(|d| {
                                            if let Some(d) = d {
                                                d.fallthrough = Distribution::Fixed {
                                                    variant: value.clone(),
                                                };
                                            }
                                        })
                                }
                            >
                                {variants
                                    .get()
                                    .into_iter()
                                    .map(|option| {
                                        let selected = option.key == variant;
                                        let (value, text) = (option.key.clone(), option.key);
                                        view! {
                                            <option value=value selected=selected>
                                                {text}
                                            </option>
                                        }
                                    })
                                    .collect_view()}
                            </select>
                        }
                            .into_any()
                    }
                    Distribution::Rollout { weights, .. } => {
                        view! {
                            <RolloutEditor
                                weights=weights
                                variants=variants
                                draft=draft
                                can_write=can_write
                            />
                        }
                            .into_any()
                    }
                }}
            </div>
        </div>
    }
}

#[component]
fn RolloutEditor(
    weights: Vec<WeightedVariant>,
    variants: Signal<Vec<Variant>>,
    draft: RwSignal<Option<Draft>>,
    can_write: Signal<bool>,
) -> impl IntoView {
    let total: u64 = weights.iter().map(|w| u64::from(w.weight)).sum();
    let balanced = total == u64::from(TOTAL_WEIGHT);

    // With exactly two variants a single slider is unambiguous: whatever the
    // first one does not take, the second one does. More than two and the
    // interactions stop being expressible in one control.
    let slider = weights.len() == 2;
    let first_percent = weights.first().map(|w| percent(w.weight)).unwrap_or(0.0);

    let set_split = move |value: f64| {
        draft.update(|d| {
            let Some(d) = d else { return };
            if let Distribution::Rollout { weights, .. } = &mut d.fallthrough
                && weights.len() == 2
            {
                let first = (value / 100.0 * f64::from(TOTAL_WEIGHT)).round() as u32;
                weights[0].weight = first.min(TOTAL_WEIGHT);
                weights[1].weight = TOTAL_WEIGHT - weights[0].weight;
            }
        })
    };

    let bar = weights.clone();
    // Three views read the weights and each closure would otherwise take
    // ownership of the one Vec.
    let rows = weights.clone();
    let slider_label = weights.first().map(|w| w.variant.clone()).unwrap_or_default();

    view! {
        <div class="stack" style="gap:var(--space-4)">
            <div class="dist" role="img" aria-label="Traffic split">
                {bar
                    .into_iter()
                    .map(|entry| {
                        let share = percent(entry.weight);
                        let label = if share >= 8.0 {
                            format!("{} {}", entry.variant, format_percent(share))
                        } else {
                            String::new()
                        };
                        let class = part_class(&variants.get(), &entry.variant);
                        view! {
                            <div
                                class=class
                                style:flex-grow=entry.weight.to_string()
                                title=format!("{} — {}", entry.variant, format_percent(share))
                            >
                                {label}
                            </div>
                        }
                    })
                    .collect_view()}
            </div>

            <Show when=move || slider>
                <div class="field">
                    <label class="label" for="rollout">
                        {format!("{slider_label} share")}
                    </label>
                    <div class="row" style="gap:var(--space-4)">
                        <input
                            id="rollout"
                            class="slider"
                            type="range"
                            min="0"
                            max="100"
                            step="0.5"
                            disabled=move || !can_write.get()
                            prop:value=first_percent.to_string()
                            on:input=move |e| {
                                if let Ok(value) = event_target_value(&e).parse::<f64>() {
                                    set_split(value);
                                }
                            }
                        />
                        <span
                            class="badge badge--accent"
                            style="font-variant-numeric:tabular-nums;min-width:76px;justify-content:center"
                        >
                            {format_percent(first_percent)}
                        </span>
                    </div>
                </div>
            </Show>

            <Show when=move || !slider>
                <div class="stack" style="gap:var(--space-2)">
                    {rows
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            let share = percent(entry.weight);
                            view! {
                                <div class="row" style="gap:var(--space-3)">
                                    <code style="min-width:110px">{entry.variant.clone()}</code>
                                    <input
                                        class="input"
                                        type="number"
                                        min="0"
                                        max="100"
                                        step="0.001"
                                        style="max-width:120px"
                                        disabled=move || !can_write.get()
                                        prop:value=share.to_string()
                                        on:change=move |e| {
                                            if let Ok(value) = event_target_value(&e).parse::<f64>()
                                            {
                                                draft
                                                    .update(|d| {
                                                        if let Some(d) = d
                                                            && let Distribution::Rollout { weights, .. } = &mut d
                                                                .fallthrough
                                                            && let Some(entry) = weights.get_mut(index)
                                                        {
                                                            entry
                                                                .weight = (value / 100.0 * f64::from(TOTAL_WEIGHT))
                                                                .round()
                                                                .clamp(0.0, f64::from(TOTAL_WEIGHT)) as u32;
                                                        }
                                                    })
                                            }
                                        }
                                    />
                                    <span class="hint">"%"</span>
                                </div>
                            }
                        })
                        .collect_view()}
                </div>
            </Show>

            <Show when=move || !balanced>
                <span class="field-error">
                    {format!("Shares add up to {}, not 100%.", format_percent(percent_u64(total)))}
                </span>
            </Show>
        </div>
    }
}

#[component]
fn Preview(
    draft: RwSignal<Option<Draft>>,
    variants: Signal<Vec<Variant>>,
    flag_key: Signal<String>,
) -> impl IntoView {
    let subject = RwSignal::new("user-42".to_owned());
    let attributes = RwSignal::new("plan=pro\ncountry=ES".to_owned());

    let candidate = Memo::new(move |_| {
        draft.get().map(|d| flagforge_core::Flag {
            key: flag_key.get(),
            variants: variants.get(),
            enabled: d.enabled,
            off_variant: d.off_variant,
            fallthrough: d.fallthrough,
            rules: d.rules,
            version: d.version,
        })
    });

    let context = Memo::new(move |_| {
        let mut ctx = EvaluationContext::new(subject.get());
        for line in attributes.get().lines() {
            if let Some((name, value)) = line.split_once('=') {
                let (name, value) = (name.trim(), value.trim());
                if name.is_empty() {
                    continue;
                }
                // Numbers stay numbers so `>=` comparisons behave the way the
                // person typing them expects.
                let parsed = match value.parse::<f64>() {
                    Ok(number) => AttributeValue::Number(number),
                    Err(_) => match value {
                        "true" => AttributeValue::Bool(true),
                        "false" => AttributeValue::Bool(false),
                        other => AttributeValue::String(other.to_owned()),
                    },
                };
                ctx = ctx.with(name, parsed);
            }
        }
        ctx
    });

    let decision = Memo::new(move |_| {
        candidate.get().map(|flag| flagforge_core::evaluate(&flag, &context.get(), PREVIEW_SALT))
    });

    // Aggregate simulation. Unlike a single subject's assignment, the split
    // across a population does not depend on the salt, so these numbers are
    // exactly what production will do.
    let simulation = Memo::new(move |_| {
        let Some(flag) = candidate.get() else { return Vec::new() };
        let base = context.get();

        let mut tally: Vec<(String, u32)> = Vec::new();
        for i in 0..SIMULATED_SUBJECTS {
            let mut ctx = base.clone();
            ctx.key = format!("sim-{i}");
            let outcome = flagforge_core::evaluate(&flag, &ctx, PREVIEW_SALT);
            let label = outcome.variant.unwrap_or_else(|| "error".to_owned());
            match tally.iter_mut().find(|(name, _)| *name == label) {
                Some((_, count)) => *count += 1,
                None => tally.push((label, 1)),
            }
        }
        // Biggest share first, so the bar reads left to right by weight.
        // Ordered by the flag's own variant list, so this bar and the editor's
        // read left-to-right the same way.
        let order = variants.get();
        tally.sort_by_key(|(name, _)| {
            order.iter().position(|v| v.key == *name).unwrap_or(usize::MAX)
        });
        tally
    });

    view! {
        <div class="card">
            <div class="card__header">
                <h2 class="card__title">"Preview"</h2>
                <span class="hint">
                    "Runs the same evaluation engine as the server, here in your browser."
                </span>
            </div>

            <div class="card__body stack">
                <div class="row" style="gap:var(--space-4);align-items:flex-start">
                    <div class="field" style="flex:1;min-width:180px">
                        <label class="label" for="preview-key">
                            "Context key"
                        </label>
                        <input
                            id="preview-key"
                            class="input input--mono"
                            prop:value=move || subject.get()
                            on:input=move |e| subject.set(event_target_value(&e))
                        />
                        <span class="hint">"What rollouts bucket on."</span>
                    </div>

                    <div class="field" style="flex:1;min-width:180px">
                        <label class="label" for="preview-attributes">
                            "Attributes"
                        </label>
                        <textarea
                            id="preview-attributes"
                            class="textarea input--mono"
                            prop:value=move || attributes.get()
                            on:input=move |e| attributes.set(event_target_value(&e))
                        ></textarea>
                        <span class="hint">"One "<code>"name=value"</code>" per line."</span>
                    </div>
                </div>

                <div class="preview">
                    {move || {
                        decision
                            .get()
                            .map(|outcome| {
                                let (badge, verdict) = match &outcome.reason {
                                    Reason::Off => ("badge badge--off", "flag is off".to_owned()),
                                    Reason::TargetMatch { index, .. } => {
                                        ("badge badge--accent", format!("matched rule {}", index + 1))
                                    }
                                    Reason::Fallthrough => {
                                        ("badge badge--info", "no rule matched".to_owned())
                                    }
                                    Reason::FlagNotFound => {
                                        ("badge badge--danger", "flag not found".to_owned())
                                    }
                                    Reason::Error { message } => {
                                        ("badge badge--danger", message.clone())
                                    }
                                };
                                view! {
                                    <div class="preview__verdict">
                                        <span class="cell-secondary">"Serves"</span>
                                        <code style="font-weight:620;font-size:14px">
                                            {outcome
                                                .variant
                                                .clone()
                                                .unwrap_or_else(|| "—".to_owned())}
                                        </code>
                                        <span class="cell-secondary">
                                            {serde_json::to_string(&outcome.value)
                                                .unwrap_or_default()}
                                        </span>
                                        <span class=badge>{verdict}</span>
                                    </div>
                                }
                            })
                    }}

                    <div class="stack" style="gap:var(--space-2)">
                        <span class="label">
                            {format!("Across {SIMULATED_SUBJECTS} simulated subjects")}
                        </span>
                        <div class="dist" role="img" aria-label="Simulated distribution">
                            {move || {
                                simulation
                                    .get()
                                    .into_iter()
                                    .map(|(variant, count)| {
                                        let share = f64::from(count)
                                            / f64::from(SIMULATED_SUBJECTS) * 100.0;
                                        let class = part_class(&variants.get(), &variant);
                                        view! {
                                            <div
                                                class=class
                                                style:flex-grow=count.to_string()
                                                title=format!("{variant}: {count}")
                                            >
                                                {if share >= 8.0 {
                                                    format!("{variant} {share:.1}%")
                                                } else {
                                                    String::new()
                                                }}
                                            </div>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </div>
                        <span class="hint">
                            "The split is exact. Which side one specific user lands on depends on the environment's bucketing salt, which never leaves the server."
                        </span>
                    </div>
                </div>
            </div>
        </div>
    }
}

/// Archiving and deletion.
///
/// Separated and visually marked because the two are not equivalent: archiving
/// stops a flag being served but keeps its history, while deleting removes it
/// from every environment at once and cannot be undone.
#[component]
fn DangerZone(project: Signal<String>, flag_key: Signal<String>) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();
    let navigate = leptos_router::hooks::use_navigate();
    let busy = RwSignal::new(false);

    let archive = move |_| {
        let (Some(token), project, flag) =
            (session.token_untracked(), project.get_untracked(), flag_key.get_untracked())
        else {
            return;
        };
        busy.set(true);

        leptos::task::spawn_local(async move {
            let result = api::archive_flag(&token, &project, &flag, true).await;
            busy.set(false);
            match result {
                Ok(_) => toaster.success(format!("{flag} archived — SDKs no longer receive it")),
                Err(error) => toaster.error("Could not archive the flag", &error),
            }
        });
    };

    let delete = move |_| {
        let (Some(token), project, flag) =
            (session.token_untracked(), project.get_untracked(), flag_key.get_untracked())
        else {
            return;
        };
        let navigate = navigate.clone();
        busy.set(true);

        leptos::task::spawn_local(async move {
            let result = api::delete_flag(&token, &project, &flag).await;
            busy.set(false);
            match result {
                Ok(()) => {
                    toaster.success(format!("{flag} deleted"));
                    navigate(&format!("/projects/{project}"), Default::default());
                }
                Err(error) => toaster.error("Could not delete the flag", &error),
            }
        });
    };

    view! {
        <div class="card">
            <div class="card__header">
                <h2 class="card__title">"Danger zone"</h2>
            </div>
            <div class="card__body stack">
                <div class="row row--between">
                    <div>
                        <div style="font-weight:540">"Archive this flag"</div>
                        <span class="hint">
                            "Hides it from lists and stops serving it, keeping the audit history. Reversible."
                        </span>
                    </div>
                    <button
                        class="btn btn--secondary btn--sm"
                        disabled=move || busy.get()
                        on:click=archive
                    >
                        "Archive"
                    </button>
                </div>

                <div class="row row--between">
                    <div>
                        <div style="font-weight:540">"Delete this flag"</div>
                        <span class="hint">
                            "Removes it from every environment immediately. SDKs fall back to their own defaults."
                        </span>
                    </div>
                    <ConfirmButton
                        label="Delete"
                        confirm_label="Delete permanently"
                        on_confirm=Callback::new(delete)
                    />
                </div>
            </div>
        </div>
    }
}

// ------------------------------------------------------------------ helpers --

const OPERATORS: &[(&str, &str)] = &[
    ("in", "is any of"),
    ("not_in", "is none of"),
    ("contains", "contains"),
    ("not_contains", "does not contain"),
    ("starts_with", "starts with"),
    ("ends_with", "ends with"),
    ("greater_than", ">"),
    ("greater_than_or_equal", "≥"),
    ("less_than", "<"),
    ("less_than_or_equal", "≤"),
    ("matches", "matches regex"),
    ("not_matches", "does not match regex"),
    ("semver_equal", "version ="),
    ("semver_greater_than", "version >"),
    ("semver_less_than", "version <"),
    ("exists", "is present"),
    ("not_exists", "is absent"),
];

/// Maps an operator to its wire value.
///
/// Goes through serde rather than a hand-written match, so the strings in
/// `OPERATORS` can never drift from what the API actually accepts.
fn operator_key(operator: Operator) -> &'static str {
    let serialized = serde_json::to_value(operator)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();

    OPERATORS
        .iter()
        .find(|(value, _)| *value == serialized)
        .map(|(value, _)| *value)
        .unwrap_or("in")
}

fn operator_from(raw: &str) -> Option<Operator> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok()
}

fn parse_values(raw: &str) -> Vec<AttributeValue> {
    raw.split(',')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(|piece| match piece.parse::<f64>() {
            Ok(number) => AttributeValue::Number(number),
            Err(_) => AttributeValue::String(piece.to_owned()),
        })
        .collect()
}

fn even_split(variants: &[Variant]) -> Vec<WeightedVariant> {
    if variants.is_empty() {
        return Vec::new();
    }

    let share = TOTAL_WEIGHT / variants.len() as u32;
    let mut weights: Vec<_> = variants
        .iter()
        .map(|variant| WeightedVariant { variant: variant.key.clone(), weight: share })
        .collect();

    // Integer division loses a few units; give the remainder to the first
    // variant so the total is always exactly TOTAL_WEIGHT.
    let assigned: u32 = share * variants.len() as u32;
    weights[0].weight += TOTAL_WEIGHT - assigned;
    weights
}

/// Colour class for a variant, keyed to its index in the flag definition.
fn part_class(variants: &[Variant], key: &str) -> String {
    let index = variants.iter().position(|v| v.key == key).unwrap_or(0);
    format!("dist__part dist__part--{}", index % 4)
}

fn percent(weight: u32) -> f64 {
    f64::from(weight) / f64::from(TOTAL_WEIGHT) * 100.0
}

fn percent_u64(weight: u64) -> f64 {
    weight as f64 / f64::from(TOTAL_WEIGHT) * 100.0
}

fn format_percent(value: f64) -> String {
    if (value - value.round()).abs() < 0.001 {
        format!("{}%", value.round())
    } else {
        format!("{value:.2}%")
    }
}

/// UUID from the browser's own CSPRNG.
///
/// `crypto.randomUUID` rather than the `uuid` crate's v4: it avoids pulling a
/// randomness backend into the WASM build for one call site.
fn new_uuid() -> uuid::Uuid {
    let generated = window().crypto().ok().map(|crypto| crypto.random_uuid()).unwrap_or_default();

    generated.parse().unwrap_or(uuid::Uuid::nil())
}
