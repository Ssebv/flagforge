//! SDK keys for an environment.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{
    self, Load, NewKey,
    models::{ApiKey, Environment},
};
use crate::app::{PageHeader, fixed};
use crate::components::{
    ConfirmButton, CopyButton, Empty, Failure, Icon, Modal, SkeletonRows, defer, relative_time,
};
use crate::session::use_session;
use crate::toast::use_toaster;

#[component]
pub fn Keys() -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();
    let params = use_params_map();

    let project_key = move || params.read().get("project").unwrap_or_default();

    let environments = RwSignal::new(Load::<Vec<Environment>>::Loading);
    let selected = RwSignal::new(Option::<String>::None);
    let keys = RwSignal::new(Load::<Vec<ApiKey>>::Loading);
    let creating = RwSignal::new(false);
    // Held until dismissed: this is the only time the secret is ever visible.
    let revealed = RwSignal::new(Option::<(String, String)>::None);

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

    let load_keys = move || {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected.get_untracked() else { return };
        keys.set(Load::Loading);
        leptos::task::spawn_local(async move {
            keys.set(api::list_keys(&token, &project, &environment).await.into());
        });
    };

    Effect::new(move |_| {
        let _ = project_key();
        load_environments();
    });

    Effect::new(move |_| {
        let _ = selected.get();
        load_keys();
    });

    let can_administer = move || session.identity().is_some_and(|me| me.user.can_administer());

    let revoke = move |id: String| {
        let (Some(token), project) = (session.token_untracked(), project_key()) else { return };
        let Some(environment) = selected.get_untracked() else { return };

        leptos::task::spawn_local(async move {
            match api::revoke_key(&token, &project, &environment, &id).await {
                Ok(()) => {
                    toaster.success("Key revoked — it stops working on the next request");
                    load_keys();
                }
                Err(error) => toaster.error("Could not revoke the key", &error),
            }
        });
    };

    view! {
        <PageHeader
            title=fixed("SDK keys")
            lead=fixed("What your services present to the evaluation API.")
        >
            <A
                href=move || format!("/projects/{}", project_key())
                attr:class="btn btn--ghost"
            >
                <Icon name="back" />
                "Back"
            </A>
            <Show when=can_administer>
                <button class="btn btn--primary" on:click=move |_| creating.set(true)>
                    <Icon name="plus" />
                    "New key"
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
                                            selected.get().as_deref() == Some(key.as_str())
                                        })
                                    };
                                    view! {
                                        <button
                                            class="tab"
                                            role="tab"
                                            type="button"
                                            aria-selected=move || active.get().to_string()
                                            on:click=move |_| selected.set(Some(key.clone()))
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

            <Show when=move || !can_administer()>
                <div class="callout">
                    <div style="width:16px;height:16px;flex:none">
                        <Icon name="alert" />
                    </div>
                    <span>"Only owners and admins can manage SDK keys."</span>
                </div>
            </Show>

            {move || match keys.get() {
                Load::Loading => {
                    view! { <div class="card"><SkeletonRows rows=2 /></div> }.into_any()
                }
                Load::Failed(error) => {
                    view! {
                        <div class="card">
                            <Failure error=error on_retry=Callback::new(move |_| load_keys()) />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) if list.is_empty() => {
                    view! {
                        <div class="card">
                            <Empty
                                icon="key"
                                title="No keys in this environment"
                                text="A key is scoped to one environment and can only evaluate flags — it cannot change them."
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
                                            <th>"Name"</th>
                                            <th>"Key"</th>
                                            <th>"Scope"</th>
                                            <th>"Last used"</th>
                                            <th style="text-align:right">"Status"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {list
                                            .into_iter()
                                            .map(|key| {
                                                let id = key.id.clone();
                                                let revoked = key.revoked_at.is_some();
                                                // Built here rather than inside the view: `Show`
                                                // children must be re-runnable, and moving the id
                                                // into a closure there would make them run-once.
                                                let revoke_this = Callback::new(move |_| {
                                                    revoke(id.clone())
                                                });
                                                let scope_class = if key.scope == "server" {
                                                    "badge badge--accent"
                                                } else {
                                                    "badge badge--info"
                                                };
                                                view! {
                                                    <tr>
                                                        <td style="font-weight:540">{key.name}</td>
                                                        <td>
                                                            <code class="cell-key">
                                                                {format!("{}…", key.prefix)}
                                                            </code>
                                                        </td>
                                                        <td>
                                                            <span class=scope_class>{key.scope}</span>
                                                        </td>
                                                        <td class="cell-secondary">
                                                            {key
                                                                .last_used_at
                                                                .as_deref()
                                                                .map(relative_time)
                                                                .unwrap_or_else(|| "never".to_owned())}
                                                        </td>
                                                        <td class="cell-actions">
                                                            <Show
                                                                when=move || !revoked
                                                                fallback=|| {
                                                                    view! {
                                                                        <span class="badge badge--danger">"Revoked"</span>
                                                                    }
                                                                }
                                                            >
                                                                <Show
                                                                    when=can_administer
                                                                    fallback=|| {
                                                                        view! { <span class="badge badge--on">"Active"</span> }
                                                                    }
                                                                >
                                                                    <ConfirmButton
                                                                        label="Revoke"
                                                                        confirm_label="Confirm revoke"
                                                                        on_confirm=revoke_this
                                                                    />
                                                                </Show>
                                                            </Show>
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
        </div>

        <Show when=move || creating.get()>
            <CreateKey
                project=Signal::derive(project_key)
                environment=Signal::derive(move || selected.get().unwrap_or_default())
                on_close=Callback::new(move |_| defer(move || creating.set(false)))
                on_created=Callback::new(move |created: (String, String)| {
                    creating.set(false);
                    revealed.set(Some(created));
                    load_keys();
                })
            />
        </Show>

        {move || {
            revealed
                .get()
                .map(|(name, secret)| {
                    view! {
                        <Modal
                            title="Copy this key now"
                            on_close=Callback::new(move |_| defer(move || revealed.set(None)))
                        >
                            <div class="card__body stack">
                                <div class="callout callout--accent">
                                    <div style="width:16px;height:16px;flex:none">
                                        <Icon name="alert" />
                                    </div>
                                    <span>
                                        "Only a hash of this key is stored, so this is the one and only time it can be shown."
                                    </span>
                                </div>

                                <div class="field">
                                    <span class="label">{name}</span>
                                    <div class="secret">
                                        <span style="flex:1">{secret.clone()}</span>
                                        <CopyButton value=secret label="Key" />
                                    </div>
                                </div>
                            </div>
                            <div class="card__footer">
                                <div class="spacer"></div>
                                <button
                                    class="btn btn--primary"
                                    on:click=move |_| defer(move || revealed.set(None))
                                >
                                    "I have copied it"
                                </button>
                            </div>
                        </Modal>
                    }
                })
        }}
    }
}

#[component]
fn CreateKey(
    project: Signal<String>,
    environment: Signal<String>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_created: Callback<(String, String)>,
) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let name = RwSignal::new(String::new());
    let scope = RwSignal::new("server".to_owned());
    let busy = RwSignal::new(false);

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);

        let Some(token) = session.token_untracked() else { return };
        let (project, environment, name_value, scope_value) = (
            project.get_untracked(),
            environment.get_untracked(),
            name.get_untracked(),
            scope.get_untracked(),
        );

        leptos::task::spawn_local(async move {
            let result = api::create_key(
                &token,
                &project,
                &environment,
                NewKey { name: &name_value, scope: &scope_value },
            )
            .await;

            busy.set(false);
            match result {
                Ok(created) => on_created.run((created.key.name, created.secret)),
                Err(error) => toaster.error("Could not create the key", &error),
            }
        });
    };

    view! {
        <Modal title="New SDK key" on_close=on_close>
            <form on:submit=submit>
                <div class="card__body stack">
                    <div class="field">
                        <label class="label" for="key-name">
                            "Name"
                        </label>
                        <input
                            id="key-name"
                            class="input"
                            required=true
                            placeholder="checkout-service"
                            prop:value=move || name.get()
                            on:input=move |e| name.set(event_target_value(&e))
                        />
                        <span class="hint">
                            "Names the holder, so it is obvious what breaks when you revoke it."
                        </span>
                    </div>

                    <div class="field">
                        <label class="label" for="key-scope">
                            "Scope"
                        </label>
                        <select
                            id="key-scope"
                            class="select"
                            on:change=move |e| scope.set(event_target_value(&e))
                        >
                            <option value="server">"Server — backend services"</option>
                            <option value="client">"Client — browsers and mobile apps"</option>
                        </select>
                        <span class="hint">
                            {move || {
                                if scope.get() == "server" {
                                    "Can evaluate flags and download targeting rules."
                                } else {
                                    "Can evaluate flags only. Rules name internal segments, so they are never shipped to a browser."
                                }
                            }}
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
                        "Create key"
                    </button>
                </div>
            </form>
        </Modal>
    }
}
