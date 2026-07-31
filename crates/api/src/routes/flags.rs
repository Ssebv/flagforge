//! Flag definitions and per-environment configuration.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flagforge_core::{Distribution, Rule, Variant};
use flagforge_storage::flags::ConfiguredFlag;
use flagforge_storage::models::{Flag, FlagConfig, NewAuditEntry};
use flagforge_storage::{audit, flags, projects};
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::routes::valid_key;
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateFlag {
    #[schema(example = "checkout.v2")]
    pub key: String,
    #[schema(example = "New checkout")]
    pub name: String,
    pub description: Option<String>,
    /// Defaults to the boolean `on`/`off` pair.
    #[serde(default)]
    pub variants: Option<Vec<Variant>>,
    /// Variant served while the flag is off. Defaults to `off`.
    #[serde(default)]
    pub off_variant: Option<String>,
    /// Served when the flag is on and no rule matches. Defaults to `on`.
    #[serde(default)]
    pub fallthrough: Option<Distribution>,
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_key}/flags", tag = "flags",
    security(("bearer" = [])), request_body = CreateFlag,
    params(("project_key" = String, Path, description = "Project key")),
    responses(
        (status = 201, description = "Flag created, disabled in every environment", body = Flag),
        (status = 422, description = "The configuration is not evaluable"),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(project_key): Path<String>,
    Json(body): Json<CreateFlag>,
) -> ApiResult<(StatusCode, Json<Flag>)> {
    caller.require_write()?;
    valid_key(&body.key, "key")?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;

    let template = flagforge_core::Flag::boolean(&body.key);
    let variants = body.variants.unwrap_or(template.variants);
    let off_variant = body.off_variant.unwrap_or(template.off_variant);
    let fallthrough = body.fallthrough.unwrap_or(template.fallthrough);

    // Validate the whole thing as the engine would see it, before anything is
    // written. A flag that cannot be evaluated must never reach the database.
    validate_evaluable(&body.key, &variants, &off_variant, &fallthrough, &[])?;

    let flag = flags::create_flag(
        &state.pool,
        project.id,
        &body.key,
        body.name.trim(),
        body.description.as_deref(),
        &variants,
        &off_variant,
        &fallthrough,
    )
    .await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "flag.created",
            "flag",
            format!("{project_key}/{}", flag.key),
        )
        .changing(None, Some(&flag)),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(flag)))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListFlagsQuery {
    /// Include archived flags. Off by default so the list stays useful.
    #[serde(default)]
    pub include_archived: bool,
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}/flags", tag = "flags",
    security(("bearer" = [])), params(ListFlagsQuery, ("project_key" = String, Path, description = "Project key")),
    responses((status = 200, description = "Flags in the project", body = Vec<Flag>))
)]
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(project_key): Path<String>,
    Query(query): Query<ListFlagsQuery>,
) -> ApiResult<Json<Vec<Flag>>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    Ok(Json(flags::list_flags(&state.pool, project.id, query.include_archived).await?))
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}/flags/{flag_key}", tag = "flags",
    security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("flag_key" = String, Path, description = "Flag key"),
    ),
    responses(
        (status = 200, description = "The flag definition", body = Flag),
        (status = 404, description = "No such flag"),
    )
)]
pub async fn get(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, flag_key)): Path<(String, String)>,
) -> ApiResult<Json<Flag>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    Ok(Json(flags::find_flag(&state.pool, project.id, &flag_key).await?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateFlag {
    pub name: Option<String>,
    /// Present-and-null clears the description; absent leaves it alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    pub variants: Option<Vec<Variant>>,
    /// Archiving hides a flag from lists and stops it being served.
    pub archived: Option<bool>,
}

#[utoipa::path(
    patch, path = "/api/v1/projects/{project_key}/flags/{flag_key}", tag = "flags",
    security(("bearer" = [])), request_body = UpdateFlag,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("flag_key" = String, Path, description = "Flag key"),
    ),
    responses(
        (status = 200, description = "The updated flag", body = Flag),
        (status = 422, description = "Removing a variant some environment still serves"),
    )
)]
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, flag_key)): Path<(String, String)>,
    Json(body): Json<UpdateFlag>,
) -> ApiResult<Json<Flag>> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let existing = flags::find_flag(&state.pool, project.id, &flag_key).await?;

    // Changing the variant set can orphan a reference from any environment's
    // configuration, so every environment is re-validated against the new set
    // before the write lands.
    if let Some(new_variants) = &body.variants {
        let environments = projects::list_environments(&state.pool, project.id).await?;
        for environment in environments {
            let config = flags::find_config(&state.pool, existing.id, environment.id).await?;
            validate_evaluable(
                &existing.key,
                new_variants,
                &config.off_variant,
                &config.fallthrough,
                &config.rules,
            )
            .map_err(|err| annotate(err, &environment.key))?;
        }
    }

    let updated = flags::update_flag(
        &state.pool,
        existing.id,
        body.name.as_deref().map(str::trim),
        body.description.as_ref().map(|d| d.as_deref()),
        body.variants.as_deref(),
        body.archived,
    )
    .await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "flag.updated",
            "flag",
            format!("{project_key}/{flag_key}"),
        )
        .changing(Some(&existing), Some(&updated)),
    )
    .await?;

    Ok(Json(updated))
}

#[utoipa::path(
    delete, path = "/api/v1/projects/{project_key}/flags/{flag_key}", tag = "flags",
    security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("flag_key" = String, Path, description = "Flag key"),
    ),
    responses(
        (status = 204, description = "Deleted from every environment"),
        (status = 403, description = "Requires an owner or admin"),
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, flag_key)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    // Deletion is irreversible and immediately changes what every SDK sees;
    // archiving is the reversible option and only needs write.
    caller.require_admin()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    flags::delete_flag(&state.pool, project.id, &flag_key).await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "flag.deleted",
            "flag",
            format!("{project_key}/{flag_key}"),
        ),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/flags/{flag_key}",
    tag = "flags", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("flag_key" = String, Path, description = "Flag key"),
    ),
    responses((status = 200, description = "The flag's configuration here", body = FlagConfig))
)]
pub async fn get_config(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, flag_key)): Path<(String, String, String)>,
) -> ApiResult<Json<FlagConfig>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;
    let flag = flags::find_flag(&state.pool, project.id, &flag_key).await?;

    Ok(Json(flags::find_config(&state.pool, flag.id, environment.id).await?))
}

/// Lists every flag in a project together with its configuration in one
/// environment — one request for the whole dashboard view.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/flags",
    tag = "flags", security(("bearer" = [])),
    params(
        ListFlagsQuery,
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses((status = 200, description = "Flags with their configuration here", body = Vec<ConfiguredFlag>))
)]
pub async fn list_configured(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
    Query(query): Query<ListFlagsQuery>,
) -> ApiResult<Json<Vec<ConfiguredFlag>>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    Ok(Json(
        flags::list_configured(&state.pool, project.id, environment.id, query.include_archived)
            .await?,
    ))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateConfig {
    pub enabled: bool,
    pub off_variant: String,
    pub fallthrough: Distribution,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Version you last read. When present, the write is rejected if anything
    /// changed since — the guard against two operators silently overwriting
    /// each other in production.
    #[serde(default)]
    pub expected_version: Option<i64>,
}

#[utoipa::path(
    put,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/flags/{flag_key}",
    tag = "flags", security(("bearer" = [])), request_body = UpdateConfig,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("flag_key" = String, Path, description = "Flag key"),
    ),
    responses(
        (status = 200, description = "Configuration applied", body = FlagConfig),
        (status = 409, description = "Someone else changed it first"),
        (status = 422, description = "The configuration is not evaluable"),
    )
)]
pub async fn update_config(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, flag_key)): Path<(String, String, String)>,
    Json(body): Json<UpdateConfig>,
) -> ApiResult<Json<FlagConfig>> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;
    let flag = flags::find_flag(&state.pool, project.id, &flag_key).await?;

    validate_evaluable(
        &flag.key,
        &flag.variants,
        &body.off_variant,
        &body.fallthrough,
        &body.rules,
    )?;

    let previous = flags::find_config(&state.pool, flag.id, environment.id).await.ok();

    let config = flags::upsert_config(
        &state.pool,
        flag.id,
        environment.id,
        body.enabled,
        &body.off_variant,
        &body.fallthrough,
        &body.rules,
        body.expected_version,
    )
    .await?;

    // The database trigger already notified every node, including this one.
    // Refreshing inline as well means the caller's own next read is guaranteed
    // to see its write rather than racing the notification.
    state.cache.refresh(environment.id).await;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "flag.configured",
            "flag",
            format!("{project_key}/{environment_key}/{flag_key}"),
        )
        .in_environment(environment.id)
        .changing(previous.as_ref(), Some(&config)),
    )
    .await?;

    tracing::info!(
        project = %project_key,
        environment = %environment_key,
        flag = %flag_key,
        enabled = config.enabled,
        version = config.version,
        actor = %caller.email,
        "flag configuration changed"
    );

    Ok(Json(config))
}

/// Runs the domain validator over the flag exactly as the engine would build
/// it, turning issues into a 422 with field paths.
fn validate_evaluable(
    key: &str,
    variants: &[Variant],
    off_variant: &str,
    fallthrough: &Distribution,
    rules: &[Rule],
) -> Result<(), ApiError> {
    let candidate = flagforge_core::Flag {
        key: key.to_owned(),
        variants: variants.to_vec(),
        enabled: true,
        off_variant: off_variant.to_owned(),
        fallthrough: fallthrough.clone(),
        rules: rules.to_vec(),
        version: 0,
    };

    flagforge_core::validate(&candidate).map_err(ApiError::Unprocessable)
}

/// Prefixes validation paths with the environment they came from, so a user
/// editing variants can see *which* environment still needs the old one.
fn annotate(error: ApiError, environment_key: &str) -> ApiError {
    match error {
        ApiError::Unprocessable(issues) => ApiError::Unprocessable(
            issues
                .into_iter()
                .map(|mut issue| {
                    issue.path = format!("environments.{environment_key}.{}", issue.path);
                    issue
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flagforge_core::WeightedVariant;

    #[test]
    fn a_rollout_that_does_not_sum_to_the_total_is_a_422() {
        let variants = vec![Variant::new("on", true), Variant::new("off", false)];
        let broken = Distribution::Rollout {
            weights: vec![WeightedVariant { variant: "on".into(), weight: 1 }],
            bucket_by: None,
        };

        let error = validate_evaluable("f", &variants, "off", &broken, &[]).unwrap_err();
        assert!(matches!(error, ApiError::Unprocessable(_)));
    }

    #[test]
    fn annotation_names_the_environment_that_blocks_a_variant_change() {
        let issues = vec![flagforge_core::ValidationIssue {
            path: "fallthrough.variant".into(),
            message: "`legacy` is not one of the flag's variants".into(),
        }];

        let ApiError::Unprocessable(annotated) =
            annotate(ApiError::Unprocessable(issues), "production")
        else {
            panic!("annotation must preserve the error kind");
        };

        assert_eq!(annotated[0].path, "environments.production.fallthrough.variant");
    }

    #[test]
    fn a_consistent_configuration_passes() {
        let variants = vec![Variant::new("on", true), Variant::new("off", false)];
        assert!(validate_evaluable("f", &variants, "off", &Distribution::fixed("on"), &[]).is_ok());
    }
}
