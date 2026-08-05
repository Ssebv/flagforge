//! Route views.

mod audit;
pub(crate) mod flag;
mod keys;
mod login;
mod project;
mod projects;
mod segments;

pub use audit::Audit;
pub use flag::FlagDetail;
pub use keys::Keys;
pub use login::Login;
pub use project::ProjectDetail;
pub use projects::Projects;
pub use segments::Segments;

/// Shared key suggestion, so a project, environment and flag all propose keys
/// the API will accept in exactly the same way.
pub(crate) use projects::slugify as projects_slug;

use leptos::prelude::*;
use leptos_router::components::A;

use crate::components::Icon;

#[component]
pub fn NotFound() -> impl IntoView {
    view! {
        <main class="auth">
            <div class="auth__panel" style="text-align:center">
                <div class="empty__icon" style="margin:0 auto var(--space-4)">
                    <div style="width:20px;height:20px">
                        <Icon name="search" />
                    </div>
                </div>
                <h1>"Page not found"</h1>
                <p class="auth__tagline" style="margin-bottom:var(--space-5)">
                    "That URL does not match anything in the dashboard."
                </p>
                <A href="/projects" attr:class="btn btn--primary">
                    "Back to projects"
                </A>
            </div>
        </main>
    }
}
