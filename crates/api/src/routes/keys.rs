//! SDK key management.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flagforge_storage::models::{ApiKey, KeyScope, NewAuditEntry};
use flagforge_storage::{api_keys, audit, projects};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{AuthUser, keys};
use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateKey {
    #[schema(example = "backend-production")]
    pub name: String,
    /// `server` for backends, `client` for browsers and mobile apps.
    pub scope: KeyScope,
}

/// The one and only response that contains the secret.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CreatedKey {
    #[serde(flatten)]
    pub key: ApiKey,
    /// Shown exactly once — it is stored only as a hash.
    #[schema(example = "ff_srv_yYQ2...")]
    pub secret: String,
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_key}/environments/{environment_key}/keys",
    tag = "keys", security(("bearer" = [])), request_body = CreateKey,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses(
        (status = 201, description = "Key created; the secret is only returned here", body = CreatedKey),
        (status = 403, description = "Requires an owner or admin"),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
    Json(body): Json<CreateKey>,
) -> ApiResult<(StatusCode, Json<CreatedKey>)> {
    // Minting a credential is an administrative act, not an editorial one.
    caller.require_admin()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let generated = keys::generate(body.scope);
    let key = api_keys::create(
        &state.pool,
        environment.id,
        body.name.trim(),
        &generated.hash,
        &generated.prefix,
        body.scope,
    )
    .await?;

    // Note what was created, never the secret itself.
    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "api_key.created",
            "api_key",
            key.id,
        )
        .in_environment(environment.id)
        .changing(None, Some(&key)),
    )
    .await?;

    tracing::info!(
        environment = %environment_key,
        key_prefix = %key.prefix,
        actor = %caller.email,
        "SDK key issued"
    );

    Ok((StatusCode::CREATED, Json(CreatedKey { key, secret: generated.secret })))
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}/environments/{environment_key}/keys",
    tag = "keys", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses((status = 200, description = "Keys, without their secrets", body = Vec<ApiKey>))
)]
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
) -> ApiResult<Json<Vec<ApiKey>>> {
    caller.require_admin()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    Ok(Json(api_keys::list(&state.pool, environment.id).await?))
}

#[utoipa::path(
    delete,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/keys/{key_id}",
    tag = "keys", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("key_id" = Uuid, Path, description = "Key id"),
    ),
    responses((status = 204, description = "Revoked; effective on the next request"))
)]
pub async fn revoke(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, key_id)): Path<(String, String, Uuid)>,
) -> ApiResult<StatusCode> {
    caller.require_admin()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    api_keys::revoke(&state.pool, environment.id, key_id).await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "api_key.revoked",
            "api_key",
            key_id,
        )
        .in_environment(environment.id),
    )
    .await?;

    tracing::info!(%key_id, actor = %caller.email, "SDK key revoked");

    Ok(StatusCode::NO_CONTENT)
}
