//! Projects and environments.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flagforge_storage::models::{Environment, NewAuditEntry, Project};
use flagforge_storage::{audit, projects};
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::routes::{new_salt, valid_key};
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateProject {
    /// Stable identifier used in URLs; `[A-Za-z0-9._-]`.
    #[schema(example = "checkout")]
    pub key: String,
    #[schema(example = "Checkout")]
    pub name: String,
    pub description: Option<String>,
}

#[utoipa::path(
    post, path = "/api/v1/projects", tag = "projects",
    security(("bearer" = [])), request_body = CreateProject,
    responses(
        (status = 201, description = "Project created", body = Project),
        (status = 403, description = "Insufficient role"),
        (status = 409, description = "That key is taken"),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Json(body): Json<CreateProject>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    caller.require_write()?;
    valid_key(&body.key, "key")?;

    let project = projects::create_project(
        &state.pool,
        caller.organization_id,
        &body.key,
        body.name.trim(),
        body.description.as_deref(),
    )
    .await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "project.created",
            "project",
            &project.key,
        )
        .changing(None, Some(&project)),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(project)))
}

#[utoipa::path(
    get, path = "/api/v1/projects", tag = "projects", security(("bearer" = [])),
    responses((status = 200, description = "Projects in the caller's organization", body = Vec<Project>))
)]
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
) -> ApiResult<Json<Vec<Project>>> {
    Ok(Json(projects::list_projects(&state.pool, caller.organization_id).await?))
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}", tag = "projects",
    security(("bearer" = [])), params(("project_key" = String, Path, description = "Project key")),
    responses(
        (status = 200, description = "The project", body = Project),
        (status = 404, description = "No such project in this organization"),
    )
)]
pub async fn get(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(project_key): Path<String>,
) -> ApiResult<Json<Project>> {
    Ok(Json(projects::find_project(&state.pool, caller.organization_id, &project_key).await?))
}

#[utoipa::path(
    delete, path = "/api/v1/projects/{project_key}", tag = "projects",
    security(("bearer" = [])), params(("project_key" = String, Path, description = "Project key")),
    responses(
        (status = 204, description = "Deleted along with its environments and flags"),
        (status = 403, description = "Requires an owner or admin"),
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(project_key): Path<String>,
) -> ApiResult<StatusCode> {
    // Deleting a project takes every flag in every environment with it, so it
    // is gated harder than an ordinary write.
    caller.require_admin()?;

    projects::delete_project(&state.pool, caller.organization_id, &project_key).await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "project.deleted",
            "project",
            &project_key,
        ),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateEnvironment {
    #[schema(example = "production")]
    pub key: String,
    #[schema(example = "Production")]
    pub name: String,
    /// Marks the environment as production-like. Purely informational today,
    /// but it is what a future "require approval" policy would key on.
    #[serde(default)]
    pub is_production: bool,
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_key}/environments", tag = "environments",
    security(("bearer" = [])), request_body = CreateEnvironment,
    params(("project_key" = String, Path, description = "Project key")),
    responses(
        (status = 201, description = "Environment created", body = Environment),
        (status = 409, description = "That key is taken in this project"),
    )
)]
pub async fn create_environment(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(project_key): Path<String>,
    Json(body): Json<CreateEnvironment>,
) -> ApiResult<(StatusCode, Json<Environment>)> {
    caller.require_write()?;
    valid_key(&body.key, "key")?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;

    // The salt is generated here and never exposed: it is what makes rollout
    // membership unguessable from outside.
    let environment = projects::create_environment(
        &state.pool,
        project.id,
        &body.key,
        body.name.trim(),
        &new_salt(),
        body.is_production,
    )
    .await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "environment.created",
            "environment",
            format!("{}/{}", project.key, environment.key),
        )
        .in_environment(environment.id)
        .changing(None, Some(&environment)),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(environment)))
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}/environments", tag = "environments",
    security(("bearer" = [])), params(("project_key" = String, Path, description = "Project key")),
    responses((status = 200, description = "Environments in the project", body = Vec<Environment>))
)]
pub async fn list_environments(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(project_key): Path<String>,
) -> ApiResult<Json<Vec<Environment>>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    Ok(Json(projects::list_environments(&state.pool, project.id).await?))
}

#[utoipa::path(
    delete, path = "/api/v1/projects/{project_key}/environments/{environment_key}",
    tag = "environments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses(
        (status = 204, description = "Deleted along with its keys and configurations"),
        (status = 403, description = "Requires an owner or admin"),
    )
)]
pub async fn delete_environment(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    caller.require_admin()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    projects::delete_environment(&state.pool, project.id, &environment_key).await?;

    // Every SDK key for this environment died with it; drop the snapshot too
    // so a cached copy cannot outlive the thing it describes.
    state.cache.refresh(environment.id).await;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "environment.deleted",
            "environment",
            format!("{project_key}/{environment_key}"),
        ),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
