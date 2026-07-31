//! Change history.

use leptos::prelude::*;
use serde_json::Value;

use crate::api::{self, Load, models::AuditEntry};
use crate::app::{PageHeader, fixed};
use crate::components::{Empty, Failure, Icon, SkeletonRows, relative_time};
use crate::session::use_session;

const PAGE_SIZE: u32 = 40;

#[component]
pub fn Audit() -> impl IntoView {
    let session = use_session();

    let entries = RwSignal::new(Load::<Vec<AuditEntry>>::Loading);
    let cursor = RwSignal::new(Option::<i64>::None);
    let loading_more = RwSignal::new(false);
    let expanded = RwSignal::new(Option::<i64>::None);

    let load_first = move || {
        let Some(token) = session.token_untracked() else { return };
        entries.set(Load::Loading);
        leptos::task::spawn_local(async move {
            match api::audit(&token, PAGE_SIZE, None).await {
                Ok(page) => {
                    cursor.set(page.next_cursor);
                    entries.set(Load::Ready(page.entries));
                }
                Err(error) => entries.set(Load::Failed(error)),
            }
        });
    };

    let load_more = move |_| {
        let (Some(token), Some(before)) = (session.token_untracked(), cursor.get_untracked())
        else {
            return;
        };
        if loading_more.get_untracked() {
            return;
        }
        loading_more.set(true);

        leptos::task::spawn_local(async move {
            if let Ok(page) = api::audit(&token, PAGE_SIZE, Some(before)).await {
                cursor.set(page.next_cursor);
                entries.update(|state| {
                    if let Load::Ready(list) = state {
                        list.extend(page.entries);
                    }
                });
            }
            loading_more.set(false);
        });
    };

    Effect::new(move |_| load_first());

    let can_administer = move || session.identity().is_some_and(|me| me.user.can_administer());

    view! {
        <PageHeader
            title=fixed("Audit log")
            lead=fixed("Who changed what, and what it looked like before.")
        />

        <div class="content stack">
            <Show when=move || !can_administer()>
                <div class="callout">
                    <div style="width:16px;height:16px;flex:none">
                        <Icon name="alert" />
                    </div>
                    <span>"The audit log is visible to owners and admins."</span>
                </div>
            </Show>

            {move || match entries.get() {
                Load::Loading => {
                    view! { <div class="card"><SkeletonRows rows=5 /></div> }.into_any()
                }
                Load::Failed(error) => {
                    view! {
                        <div class="card">
                            <Failure error=error on_retry=Callback::new(move |_| load_first()) />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) if list.is_empty() => {
                    view! {
                        <div class="card">
                            <Empty
                                icon="history"
                                title="Nothing recorded yet"
                                text="Every change to a project, flag or key lands here automatically."
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
                                            <th>"When"</th>
                                            <th>"Who"</th>
                                            <th>"Action"</th>
                                            <th>"Resource"</th>
                                            <th style="text-align:right"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {list
                                            .into_iter()
                                            .map(|entry| {
                                                view! { <AuditRow entry=entry expanded=expanded /> }
                                            })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>

                            <Show when=move || cursor.get().is_some()>
                                <div class="card__footer">
                                    <div class="spacer"></div>
                                    <button
                                        class="btn btn--secondary btn--sm"
                                        disabled=move || loading_more.get()
                                        on:click=load_more
                                    >
                                        <Show when=move || loading_more.get()>
                                            <span class="spinner"></span>
                                        </Show>
                                        "Load older entries"
                                    </button>
                                    <div class="spacer"></div>
                                </div>
                            </Show>
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn AuditRow(entry: AuditEntry, expanded: RwSignal<Option<i64>>) -> impl IntoView {
    let id = entry.id;
    let is_open = move || expanded.get() == Some(id);
    let has_diff = entry.before.is_some() || entry.after.is_some();
    let (before, after) = (entry.before.clone(), entry.after.clone());

    view! {
        <tr>
            <td class="cell-secondary" title=entry.created_at.clone()>
                {relative_time(&entry.created_at)}
            </td>
            <td>{entry.actor_email}</td>
            <td>
                <span class=badge_for(&entry.action)>{entry.action.clone()}</span>
            </td>
            <td>
                <code class="cell-key">{entry.resource_id}</code>
            </td>
            <td class="cell-actions">
                <Show when=move || has_diff>
                    <button
                        class="btn btn--ghost btn--sm"
                        on:click=move |_| {
                            expanded.set(if is_open() { None } else { Some(id) })
                        }
                    >
                        {move || if is_open() { "Hide" } else { "Details" }}
                    </button>
                </Show>
            </td>
        </tr>

        <Show when=is_open>
            <tr>
                <td colspan="5" style="background:var(--surface-sunken)">
                    <div class="diff">
                        <div>
                            <span class="diff__label">"Before"</span>
                            <div class="diff__side">{render_json(&before)}</div>
                        </div>
                        <div>
                            <span class="diff__label">"After"</span>
                            <div class="diff__side">{render_json(&after)}</div>
                        </div>
                    </div>
                </td>
            </tr>
        </Show>
    }
}

/// Colour-codes the action so a destructive change stands out while scanning.
fn badge_for(action: &str) -> &'static str {
    if action.ends_with(".deleted") || action.ends_with(".revoked") {
        "badge badge--danger"
    } else if action.ends_with(".created") {
        "badge badge--on"
    } else {
        "badge badge--accent"
    }
}

fn render_json(value: &Option<Value>) -> String {
    match value {
        Some(value) => serde_json::to_string_pretty(value).unwrap_or_else(|_| "—".to_owned()),
        None => "—".to_owned(),
    }
}
