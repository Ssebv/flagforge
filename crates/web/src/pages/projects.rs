//! Project list.

use leptos::prelude::*;
use leptos_router::components::A;

use crate::api::{self, Load, NewProject, models::Project};
use crate::app::{PageHeader, fixed};
use crate::components::{Empty, Failure, Icon, Modal, SkeletonRows, defer, relative_time};
use crate::session::use_session;
use crate::toast::use_toaster;

#[component]
pub fn Projects() -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let projects = RwSignal::new(Load::<Vec<Project>>::Loading);
    let creating = RwSignal::new(false);

    let reload = move || {
        let Some(token) = session.token_untracked() else { return };
        projects.set(Load::Loading);
        leptos::task::spawn_local(async move {
            projects.set(api::list_projects(&token).await.into());
        });
    };

    Effect::new(move |_| reload());

    let can_write = move || session.identity().is_some_and(|me| me.user.can_write());

    view! {
        <PageHeader
            title=fixed("Projects")
            lead=fixed("Each project holds its own flags and environments.")
        >
            <Show when=can_write>
                <button class="btn btn--primary" on:click=move |_| creating.set(true)>
                    <Icon name="plus" />
                    "New project"
                </button>
            </Show>
        </PageHeader>

        <div class="content">
            {move || match projects.get() {
                Load::Loading => {
                    view! { <div class="card"><SkeletonRows rows=3 /></div> }.into_any()
                }
                Load::Failed(error) => {
                    view! {
                        <div class="card">
                            <Failure error=error on_retry=Callback::new(move |_| reload()) />
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) if list.is_empty() => {
                    view! {
                        <div class="card">
                            <Empty
                                icon="folder"
                                title="No projects yet"
                                text="A project groups related flags — one per service or product area works well."
                            >
                                <Show when=can_write>
                                    <button
                                        class="btn btn--primary"
                                        on:click=move |_| creating.set(true)
                                    >
                                        <Icon name="plus" />
                                        "Create your first project"
                                    </button>
                                </Show>
                            </Empty>
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(list) => {
                    view! {
                        <div class="grid">
                            {list
                                .into_iter()
                                .map(|project| view! { <ProjectCard project=project /> })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>

        <Show when=move || creating.get()>
            <CreateProject
                on_close=Callback::new(move |_| defer(move || creating.set(false)))
                on_created=Callback::new(move |name: String| {
                    creating.set(false);
                    toaster.success(format!("Project “{name}” created"));
                    reload();
                })
            />
        </Show>
    }
}

#[component]
fn ProjectCard(project: Project) -> impl IntoView {
    let href = format!("/projects/{}", project.key);

    view! {
        <A href=href attr:class="card" attr:style="display:block;text-decoration:none">
            <div class="card__body stack" style="gap:var(--space-3)">
                <div class="row" style="gap:var(--space-3)">
                    <span class="avatar" style="border-radius:var(--radius-sm)">
                        <div style="width:15px;height:15px">
                            <Icon name="folder" />
                        </div>
                    </span>
                    <div style="min-width:0">
                        <h2 style="font-size:15px">{project.name}</h2>
                        <code class="cell-secondary">{project.key}</code>
                    </div>
                </div>

                <p class="cell-secondary" style="min-height:2.4em">
                    {project
                        .description
                        .unwrap_or_else(|| "No description.".to_owned())}
                </p>

                <span class="cell-secondary">
                    {format!("Created {}", relative_time(&project.created_at))}
                </span>
            </div>
        </A>
    }
}

#[component]
fn CreateProject(
    #[prop(into)] on_close: Callback<()>,
    #[prop(into)] on_created: Callback<String>,
) -> impl IntoView {
    let session = use_session();
    let toaster = use_toaster();

    let name = RwSignal::new(String::new());
    let key = RwSignal::new(String::new());
    // Cleared once the user edits the key by hand, so we stop overwriting it.
    let key_is_derived = RwSignal::new(true);
    let busy = RwSignal::new(false);

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);

        let Some(token) = session.token_untracked() else { return };
        let (name_value, key_value) = (name.get_untracked(), key.get_untracked());

        leptos::task::spawn_local(async move {
            let result = api::create_project(
                &token,
                NewProject { key: &key_value, name: &name_value, description: None },
            )
            .await;

            busy.set(false);
            match result {
                Ok(project) => on_created.run(project.name),
                Err(error) => toaster.error("Could not create the project", &error),
            }
        });
    };

    view! {
        <Modal title="New project" on_close=on_close>
            <form on:submit=submit>
                <div class="card__body stack">
                    <div class="field">
                        <label class="label" for="project-name">
                            "Name"
                        </label>
                        <input
                            id="project-name"
                            class="input"
                            required=true
                            placeholder="Checkout"
                            prop:value=move || name.get()
                            on:input=move |e| {
                                let value = event_target_value(&e);
                                if key_is_derived.get_untracked() {
                                    key.set(slugify(&value));
                                }
                                name.set(value);
                            }
                        />
                    </div>

                    <div class="field">
                        <label class="label" for="project-key">
                            "Key"
                        </label>
                        <input
                            id="project-key"
                            class="input input--mono"
                            required=true
                            placeholder="checkout"
                            prop:value=move || key.get()
                            on:input=move |e| {
                                key_is_derived.set(false);
                                key.set(event_target_value(&e));
                            }
                        />
                        <span class="hint">
                            "Used in URLs and by the API. Letters, digits, dot, underscore and dash."
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
                        "Create project"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Mirrors the server's key rules so the suggested key is always one the API
/// will accept.
pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }

    out.trim_end_matches('-').to_owned()
}
