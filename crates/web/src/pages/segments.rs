//! Reusable audiences for one environment.
//!
//! Segments are environment-scoped, so this page is organised the way the data
//! is: pick an environment, then edit the audiences that exist inside it. There
//! is no project-wide view because there is no project-wide segment.

use std::collections::BTreeSet;

use flagforge_core::{
    AttributeValue, Condition, Operator, SegmentRollout, SegmentRule, TOTAL_WEIGHT,
};
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{
    self, Load, NewSegment, SegmentBody,
    models::{Environment, Segment},
};
use crate::app::{PageHeader, fixed};
use crate::components::{ConfirmButton, Empty, Failure, Icon, Modal, SkeletonRows, defer};
use crate::pages::flag::{OPERATORS, new_uuid, operator_from, operator_key, parse_values};
use crate::pages::projects_slug;
use crate::session::use_session;
use crate::toast::use_toaster;

/// The edit buffer for one segment.
///
/// Include and exclude lists are held as free text rather than as sets: an
/// operator pasting a column of account ids should not have to add commas, and
/// re-serialising a set on every keystroke would fight the cursor.
#[derive(Debug, Clone, PartialEq)]
struct Draft {
    key: String,
    name: String,
    included: String,
    excluded: String,
    rules: Vec<SegmentRule>,
    version: i64,
    referenced_by: Vec<String>,
}

impl Draft {
    fn from(segment: &Segment, referenced_by: Vec<String>) -> Self {
        Self {
            key: segment.key.clone(),
            name: segment.name.clone(),
            included: to_lines(&segment.included),
            excluded: to_lines(&segment.excluded),
            rules: segment.rules.clone(),
            version: segment.version,
            referenced_by,
        }
    }
}

fn to_lines(keys: &BTreeSet<String>) -> String {
    keys.iter().cloned().collect::<Vec<_>>().join("\n")
}

/// Splits on newlines and commas, so both a pasted column and a typed list
/// work without the operator having to know which one we wanted.
fn from_lines(raw: &str) -> BTreeSet<String> {
    raw.split(['\n', ','])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[component]
pub fn Segments() -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();
    let params = use_params_map();

    let project_key = move || params.read().get("project").unwrap_or_default();

    let environments = RwSignal::new(Load::<Vec<Environment>>::Loading);
    let selected_env = RwSignal::new(Option::<String>::None);
    let segments = RwSignal::new(Load::<Vec<Segment>>::Loading);
    let draft = RwSignal::new(Option::<Draft>::None);
    let creating = RwSignal::new(false);
    let saving = RwSignal::new(false);

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

    let load_segments = move || {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        segments.set(Load::Loading);
        leptos::task::spawn_local(async move {
            segments.set(api::list_segments(&token, &project, &environment).await.into());
        });
    };

    // Opening one fetches it again, because the list does not carry usage and
    // the delete button must know whether it will be refused before it is
    // pressed.
    let open = move |key: String| {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        leptos::task::spawn_local(async move {
            match api::get_segment(&token, &project, &environment, &key).await {
                Ok(loaded) => draft.set(Some(Draft::from(&loaded.segment, loaded.referenced_by))),
                Err(error) => toaster.error("Could not open the segment", &error),
            }
        });
    };

    Effect::new(move |_| {
        let _ = project_key();
        load_environments();
    });

    Effect::new(move |_| {
        let _ = selected_env.get();
        draft.set(None);
        load_segments();
    });

    let can_write = move || session.identity().is_some_and(|me| me.user.can_write());

    let save = move |_| {
        let Some(current) = draft.get_untracked() else { return };
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        if saving.get_untracked() {
            return;
        }
        saving.set(true);

        leptos::task::spawn_local(async move {
            let body = SegmentBody {
                name: current.name.clone(),
                included: from_lines(&current.included),
                excluded: from_lines(&current.excluded),
                rules: current.rules.clone(),
                expected_version: current.version,
            };

            let result = api::put_segment(&token, &project, &environment, &current.key, body).await;
            saving.set(false);

            match result {
                Ok(saved) => {
                    toaster.success("Segment saved — every flag that references it follows");
                    draft.set(Some(Draft::from(&saved, current.referenced_by.clone())));
                    load_segments();
                }
                Err(error) if error.is_conflict() => {
                    toaster.error("Someone else changed this segment — reopen it", &error);
                    open(current.key.clone());
                }
                Err(error) => toaster.error("Could not save the segment", &error),
            }
        });
    };

    let delete = move |key: String| {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected_env.get_untracked() else { return };
        leptos::task::spawn_local(async move {
            match api::delete_segment(&token, &project, &environment, &key).await {
                Ok(()) => {
                    toaster.success("Segment deleted");
                    draft.set(None);
                    load_segments();
                }
                Err(error) => toaster.error("Could not delete the segment", &error),
            }
        });
    };

    view! {
        <PageHeader
            title=fixed("Segments")
            lead=fixed(
                "Named audiences that flag rules reference, so one edit moves every flag using it.",
            )
        >
            <A href=move || format!("/projects/{}", project_key()) attr:class="btn btn--ghost">
                <Icon name="back" />
                "Back"
            </A>
            <Show when=can_write>
                <button class="btn btn--primary" on:click=move |_| creating.set(true)>
                    <Icon name="plus" />
                    "New segment"
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

            <div class="callout">
                <div style="width:16px;height:16px;flex:none">
                    <Icon name="alert" />
                </div>
                <span>
                    "A segment belongs to one environment. Defining `beta-testers` in production does not define it in staging."
                </span>
            </div>

            {move || match segments.get() {
                Load::Loading => {
                    view! { <div class="card"><SkeletonRows rows=2 /></div> }.into_any()
                }
                Load::Failed(error) => {
                    view! {
                        <div class="card">
                            <Failure error=error on_retry=Callback::new(move |_| load_segments()) />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) if list.is_empty() => {
                    view! {
                        <div class="card">
                            <Empty
                                icon="users"
                                title="No segments in this environment"
                                text="A segment names an audience once — “beta testers”, “EU accounts” — so flag rules can point at it instead of restating its conditions."
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
                                            <th>"Name"</th>
                                            <th>"Membership"</th>
                                            <th style="text-align:right"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {list
                                            .into_iter()
                                            .map(|segment| {
                                                let key = segment.key.clone();
                                                let summary = describe(&segment);
                                                let empty = segment.is_empty();
                                                view! {
                                                    <tr>
                                                        <td>
                                                            <code class="cell-key">{segment.key.clone()}</code>
                                                        </td>
                                                        <td style="font-weight:540">{segment.name}</td>
                                                        <td class="cell-secondary">
                                                            <Show
                                                                when=move || !empty
                                                                fallback=|| {
                                                                    view! {
                                                                        <span class="badge badge--danger">"Matches nobody"</span>
                                                                    }
                                                                }
                                                            >
                                                                {summary.clone()}
                                                            </Show>
                                                        </td>
                                                        <td class="cell-actions">
                                                            <button
                                                                class="btn btn--ghost btn--sm"
                                                                on:click=move |_| open(key.clone())
                                                            >
                                                                "Edit"
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
                draft
                    .get()
                    .map(|current| {
                        let key = current.key.clone();
                        let delete_this = Callback::new(move |_| delete(key.clone()));
                        view! {
                            <Editor
                                draft=draft
                                can_write=Signal::derive(can_write)
                                saving=Signal::derive(move || saving.get())
                                on_save=Callback::new(save)
                                on_delete=delete_this
                                referenced_by=current.referenced_by.clone()
                            />
                        }
                    })
            }}
        </div>

        <Show when=move || creating.get()>
            <CreateSegment
                project=Signal::derive(project_key)
                environment=Signal::derive(move || selected_env.get().unwrap_or_default())
                on_close=Callback::new(move |_| defer(move || creating.set(false)))
                on_created=Callback::new(move |key: String| {
                    creating.set(false);
                    load_segments();
                    open(key);
                })
            />
        </Show>
    }
}

/// One-line summary of how a context gets into a segment.
fn describe(segment: &Segment) -> String {
    let mut parts = Vec::new();
    if !segment.included.is_empty() {
        parts.push(format!("{} always in", segment.included.len()));
    }
    if !segment.rules.is_empty() {
        parts.push(format!(
            "{} rule{}",
            segment.rules.len(),
            if segment.rules.len() == 1 { "" } else { "s" }
        ));
    }
    if !segment.excluded.is_empty() {
        parts.push(format!("{} excluded", segment.excluded.len()));
    }
    parts.join(" · ")
}

#[component]
fn Editor(
    draft: RwSignal<Option<Draft>>,
    can_write: Signal<bool>,
    saving: Signal<bool>,
    #[prop(into)] on_save: Callback<()>,
    #[prop(into)] on_delete: Callback<()>,
    referenced_by: Vec<String>,
) -> impl IntoView {
    let rules = Signal::derive(move || draft.get().map(|d| d.rules).unwrap_or_default());
    let title = Signal::derive(move || draft.get().map(|d| d.key).unwrap_or_default());
    let blocked = !referenced_by.is_empty();
    let used_by = referenced_by.join(", ");

    view! {
        <div class="card">
            <div class="card__header">
                <h2 class="card__title">
                    "Editing "
                    <code class="cell-key">{move || title.get()}</code>
                </h2>
                <div class="spacer"></div>
                <button class="btn btn--ghost btn--sm" on:click=move |_| draft.set(None)>
                    "Close"
                </button>
            </div>

            <div class="card__body stack">
                <Show when=move || !referenced_by.is_empty()>
                    <div class="callout callout--accent">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "Referenced by " <strong>{used_by.clone()}</strong>
                            ". Saving changes what those flags serve."
                        </span>
                    </div>
                </Show>

                <div class="field">
                    <label class="label" for="segment-name">
                        "Name"
                    </label>
                    <input
                        id="segment-name"
                        class="input"
                        disabled=move || !can_write.get()
                        prop:value=move || draft.get().map(|d| d.name).unwrap_or_default()
                        on:input=move |e| {
                            let value = event_target_value(&e);
                            draft
                                .update(|d| {
                                    if let Some(d) = d {
                                        d.name = value.clone();
                                    }
                                })
                        }
                    />
                </div>

                <div class="field">
                    <label class="label" for="segment-included">
                        "Always in"
                    </label>
                    <textarea
                        id="segment-included"
                        class="input"
                        rows="3"
                        placeholder="one context key per line"
                        disabled=move || !can_write.get()
                        prop:value=move || draft.get().map(|d| d.included).unwrap_or_default()
                        on:input=move |e| {
                            let value = event_target_value(&e);
                            draft
                                .update(|d| {
                                    if let Some(d) = d {
                                        d.included = value.clone();
                                    }
                                })
                        }
                    ></textarea>
                    <span class="hint">
                        "Context keys that are members whatever the rules say."
                    </span>
                </div>

                <div class="field">
                    <label class="label" for="segment-excluded">
                        "Never in"
                    </label>
                    <textarea
                        id="segment-excluded"
                        class="input"
                        rows="3"
                        placeholder="one context key per line"
                        disabled=move || !can_write.get()
                        prop:value=move || draft.get().map(|d| d.excluded).unwrap_or_default()
                        on:input=move |e| {
                            let value = event_target_value(&e);
                            draft
                                .update(|d| {
                                    if let Some(d) = d {
                                        d.excluded = value.clone();
                                    }
                                })
                        }
                    ></textarea>
                    <span class="hint">
                        "Checked first, so an exclusion beats both the list above and every rule."
                    </span>
                </div>

                <div class="field">
                    <span class="label">"Rules"</span>
                    <span class="hint">
                        "A context in "
                        <strong>"any"</strong>
                        " of these is a member — unlike a flag rule's conditions, which must all hold."
                    </span>

                    <Show
                        when=move || !rules.get().is_empty()
                        fallback=|| {
                            view! {
                                <p class="cell-secondary" style="margin:var(--space-2) 0">
                                    "No rules — only the lists above decide membership."
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
                                            can_write=can_write
                                        />
                                    }
                                })
                                .collect_view()
                        }}
                    </Show>

                    <Show when=move || can_write.get()>
                        <button
                            class="btn btn--ghost btn--sm"
                            style="align-self:flex-start"
                            on:click=move |_| {
                                draft
                                    .update(|d| {
                                        if let Some(d) = d {
                                            d.rules
                                                .push(
                                                    SegmentRule::new(
                                                        new_uuid(),
                                                        vec![
                                                            Condition::new(
                                                                "plan",
                                                                Operator::In,
                                                                vec![AttributeValue::String("pro".into())],
                                                            ),
                                                        ],
                                                    ),
                                                );
                                        }
                                    })
                            }
                        >
                            <Icon name="plus" />
                            "Add rule"
                        </button>
                    </Show>
                </div>
            </div>

            <div class="card__footer">
                <Show when=move || can_write.get()>
                    <Show
                        when=move || !blocked
                        fallback=move || {
                            view! {
                                <span class="cell-secondary">
                                    "Referenced by a flag rule, so it cannot be deleted yet."
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
                <Show when=move || can_write.get()>
                    <button
                        class="btn btn--primary"
                        disabled=move || saving.get()
                        on:click=move |_| on_save.run(())
                    >
                        <Show when=move || saving.get()>
                            <span class="spinner"></span>
                        </Show>
                        "Save segment"
                    </button>
                </Show>
            </div>
        </div>
    }
}

#[component]
fn RuleCard(
    index: usize,
    rule: SegmentRule,
    draft: RwSignal<Option<Draft>>,
    can_write: Signal<bool>,
) -> impl IntoView {
    let rollout = rule.rollout.clone();
    let has_rollout = rollout.is_some();
    let percentage = rollout.as_ref().map(|r| r.percentage).unwrap_or(TOTAL_WEIGHT);

    view! {
        <div class="rule">
            <div class="rule__header">
                <span class="rule__order">{index + 1}</span>
                <input
                    class="input"
                    style="border:0;background:transparent;padding:2px 4px;font-weight:540"
                    placeholder="Describe this rule…"
                    aria-label=format!("Description for segment rule {}", index + 1)
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
                        aria-label=format!("Remove segment rule {}", index + 1)
                        on:click=move |_| {
                            draft
                                .update(|d| {
                                    if let Some(d) = d {
                                        d.rules.remove(index);
                                    }
                                })
                        }
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
                    <label class="condition__joiner" for=format!("segment-rollout-{index}")>
                        "Of those, admit"
                    </label>
                    <input
                        id=format!("segment-rollout-{index}")
                        class="input"
                        type="number"
                        min="0"
                        max="100"
                        step="0.1"
                        style="max-width:110px"
                        disabled=move || !can_write.get()
                        prop:value=to_percent(percentage)
                        on:change=move |e| {
                            let raw = event_target_value(&e);
                            draft
                                .update(|d| {
                                    if let Some(d) = d
                                        && let Some(rule) = d.rules.get_mut(index)
                                    {
                                        rule.rollout = from_percent(&raw)
                                            .map(|percentage| SegmentRollout {
                                                percentage,
                                                bucket_by: rule
                                                    .rollout
                                                    .as_ref()
                                                    .and_then(|r| r.bucket_by.clone()),
                                            });
                                    }
                                })
                        }
                    />
                    <span class="condition__joiner">"%"</span>
                    <span class="hint">
                        {if has_rollout {
                            "Bucketed on the segment, so the cohort holds still across every flag."
                        } else {
                            "100 % — everyone the conditions match."
                        }}
                    </span>
                </div>
            </div>
        </div>
    }
}

/// A whole-population rollout is the same thing as no rollout, so it is stored
/// as `None`: a `100 %` cohort that still hashed every context would be work
/// done to reach a foregone conclusion.
fn from_percent(raw: &str) -> Option<u32> {
    let value: f64 = raw.trim().parse().ok()?;
    let clamped = value.clamp(0.0, 100.0);
    if clamped >= 100.0 {
        return None;
    }
    Some((clamped * (f64::from(TOTAL_WEIGHT) / 100.0)).round() as u32)
}

fn to_percent(weight: u32) -> String {
    let percent = f64::from(weight) * 100.0 / f64::from(TOTAL_WEIGHT);
    if (percent - percent.round()).abs() < f64::EPSILON {
        format!("{}", percent.round())
    } else {
        format!("{percent:.1}")
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
                    aria-label="Context attribute"
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
                aria-label=format!("Comparison for `{}`", condition.attribute)
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
                            <option value=*value selected=operator_key(current_operator) == *value>
                                {*label}
                            </option>
                        }
                    })
                    .collect_view()}
            </select>

            <input
                class="input"
                placeholder="value, value, …"
                aria-label=format!("Values compared against `{}`", condition.attribute)
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
fn CreateSegment(
    project: Signal<String>,
    environment: Signal<String>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_created: Callback<String>,
) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let name = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    // Once the operator edits the key themselves, the name stops driving it.
    let key_touched = RwSignal::new(false);
    let busy = RwSignal::new(false);

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);

        let Some(token) = session.token_untracked() else { return };
        let (project, environment, name_value, key_value) = (
            project.get_untracked(),
            environment.get_untracked(),
            name.get_untracked(),
            key.get_untracked(),
        );

        leptos::task::spawn_local(async move {
            let result = api::create_segment(
                &token,
                &project,
                &environment,
                NewSegment { key: &key_value, name: &name_value, description: None },
            )
            .await;

            busy.set(false);
            match result {
                Ok(created) => on_created.run(created.key),
                Err(error) => toaster.error("Could not create the segment", &error),
            }
        });
    };

    view! {
        <Modal title="New segment" on_close=on_close>
            <form on:submit=submit>
                <div class="card__body stack">
                    <div class="field">
                        <label class="label" for="new-segment-name">
                            "Name"
                        </label>
                        <input
                            id="new-segment-name"
                            class="input"
                            required=true
                            placeholder="Beta testers"
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
                        <label class="label" for="new-segment-key">
                            "Key"
                        </label>
                        <input
                            id="new-segment-key"
                            class="input input--mono"
                            required=true
                            placeholder="beta-testers"
                            prop:value=move || key.get()
                            on:input=move |e| {
                                key_touched.set(true);
                                key.set(event_target_value(&e));
                            }
                        />
                        <span class="hint">
                            "What flag rules reference. It cannot be changed later."
                        </span>
                    </div>

                    <div class="callout">
                        <div style="width:16px;height:16px;flex:none">
                            <Icon name="alert" />
                        </div>
                        <span>
                            "Created empty — it matches nobody until you give it members."
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
                        "Create segment"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
