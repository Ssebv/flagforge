//! A/B experiments.
//!
//! Environment-scoped like segments, and for a stronger reason: an experiment
//! *is* a measurement of one environment's traffic. Its lifecycle is the state
//! machine the storage layer enforces — draft, running, stopped, one way.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flagforge_core::VariantCounts;
use flagforge_storage::models::{Experiment, ExperimentState, NewAuditEntry};
use flagforge_storage::{audit, experiments, flags, projects};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::routes::valid_key;
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateExperiment {
    #[schema(example = "checkout-cta")]
    pub key: String,
    #[schema(example = "Checkout call-to-action")]
    pub name: String,
    pub description: Option<String>,
    /// The flag whose variants are being compared.
    #[schema(example = "checkout.v2")]
    pub flag_key: String,
    /// Conversion events carrying this metric key count toward the experiment.
    #[schema(example = "order.completed")]
    pub metric_key: String,
    /// The baseline variant; every other variant is judged against it.
    #[schema(example = "off")]
    pub control_variant: String,
}

/// Presentation fields only. The measurement fields — flag, metric, control —
/// are fixed at creation: changing what a running experiment measures would
/// splice two questions into one answer, and a draft is cheap to recreate.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateExperiment {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
}

/// An experiment next to its judged results, one entry per variant.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ExperimentResults {
    pub experiment: Experiment,
    /// In the flag's variant order, zero-filled for arms nobody has reached,
    /// so every arm renders even before its first exposure. A variant that
    /// appears in the counters but not on the flag — possible after a variant
    /// rename — is appended rather than hidden.
    pub results: Vec<flagforge_core::VariantResult>,
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments",
    tag = "experiments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses((status = 200, description = "Experiments in this environment", body = Vec<Experiment>))
)]
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Experiment>>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    Ok(Json(experiments::list_experiments(&state.pool, environment.id).await?))
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments",
    tag = "experiments", security(("bearer" = [])), request_body = CreateExperiment,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses(
        (status = 201, description = "Experiment created as a draft", body = Experiment),
        (status = 400, description = "Unknown flag, archived flag, or a control that is not one of its variants"),
        (status = 409, description = "An experiment with that key already exists here"),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
    Json(body): Json<CreateExperiment>,
) -> ApiResult<(StatusCode, Json<Experiment>)> {
    caller.require_write()?;
    valid_key(&body.key, "key")?;
    valid_key(&body.metric_key, "metric_key")?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let flag = flags::find_flag(&state.pool, project.id, &body.flag_key).await?;
    if flag.archived {
        return Err(ApiError::BadRequest(format!(
            "flag `{}` is archived; an experiment would measure traffic it no longer serves",
            body.flag_key
        )));
    }
    if !flag.variants.iter().any(|v| v.key == body.control_variant) {
        return Err(ApiError::BadRequest(format!(
            "`{}` is not a variant of `{}` (it has: {})",
            body.control_variant,
            body.flag_key,
            flag.variants.iter().map(|v| v.key.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }

    let experiment = experiments::create_experiment(
        &state.pool,
        environment.id,
        flag.id,
        &body.key,
        &body.name,
        body.description.as_deref(),
        &body.metric_key,
        &body.control_variant,
    )
    .await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "experiment.created",
            "experiment",
            format!("{project_key}/{environment_key}/{}", body.key),
        )
        .in_environment(environment.id)
        .changing(None, Some(&experiment)),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(experiment)))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments/{experiment_key}",
    tag = "experiments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("experiment_key" = String, Path, description = "Experiment key"),
    ),
    responses((status = 200, description = "The experiment", body = Experiment))
)]
pub async fn get(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, experiment_key)): Path<(String, String, String)>,
) -> ApiResult<Json<Experiment>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    Ok(Json(experiments::find_experiment(&state.pool, environment.id, &experiment_key).await?))
}

#[utoipa::path(
    patch,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments/{experiment_key}",
    tag = "experiments", security(("bearer" = [])), request_body = UpdateExperiment,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("experiment_key" = String, Path, description = "Experiment key"),
    ),
    responses((status = 200, description = "Experiment updated", body = Experiment))
)]
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, experiment_key)): Path<(String, String, String)>,
    Json(body): Json<UpdateExperiment>,
) -> ApiResult<Json<Experiment>> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let previous =
        experiments::find_experiment(&state.pool, environment.id, &experiment_key).await?;
    let experiment = experiments::update_experiment(
        &state.pool,
        environment.id,
        &experiment_key,
        body.name.as_deref(),
        body.description.as_ref().map(|d| d.as_deref()),
    )
    .await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "experiment.updated",
            "experiment",
            format!("{project_key}/{environment_key}/{experiment_key}"),
        )
        .in_environment(environment.id)
        .changing(Some(&previous), Some(&experiment)),
    )
    .await?;

    Ok(Json(experiment))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments/{experiment_key}/start",
    tag = "experiments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("experiment_key" = String, Path, description = "Experiment key"),
    ),
    responses(
        (status = 200, description = "Now running; SDKs will start recording", body = Experiment),
        (status = 409, description = "Not a draft — a stopped experiment cannot restart"),
    )
)]
pub async fn start(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, experiment_key)): Path<(String, String, String)>,
) -> ApiResult<Json<Experiment>> {
    transition(
        state,
        caller,
        project_key,
        environment_key,
        experiment_key,
        ExperimentState::Running,
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments/{experiment_key}/stop",
    tag = "experiments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("experiment_key" = String, Path, description = "Experiment key"),
    ),
    responses(
        (status = 200, description = "Stopped; the results are final", body = Experiment),
        (status = 409, description = "Only a running experiment can stop"),
    )
)]
pub async fn stop(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, experiment_key)): Path<(String, String, String)>,
) -> ApiResult<Json<Experiment>> {
    transition(
        state,
        caller,
        project_key,
        environment_key,
        experiment_key,
        ExperimentState::Stopped,
    )
    .await
}

async fn transition(
    state: AppState,
    caller: AuthUser,
    project_key: String,
    environment_key: String,
    experiment_key: String,
    to: ExperimentState,
) -> ApiResult<Json<Experiment>> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let (experiment, action) = match to {
        ExperimentState::Running => (
            experiments::start_experiment(&state.pool, environment.id, &experiment_key).await?,
            "experiment.started",
        ),
        _ => (
            experiments::stop_experiment(&state.pool, environment.id, &experiment_key).await?,
            "experiment.stopped",
        ),
    };

    // Starting and stopping both change what SDKs should record, and the
    // snapshot is how they find out. Refreshing inline means the caller's next
    // read sees its own write rather than racing the trigger's notification.
    state.cache.refresh(environment.id).await;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            action,
            "experiment",
            format!("{project_key}/{environment_key}/{experiment_key}"),
        )
        .in_environment(environment.id)
        .changing(None, Some(&experiment)),
    )
    .await?;

    Ok(Json(experiment))
}

#[utoipa::path(
    delete,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments/{experiment_key}",
    tag = "experiments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("experiment_key" = String, Path, description = "Experiment key"),
    ),
    responses(
        (status = 204, description = "Experiment and its counters deleted"),
        (status = 409, description = "Running — stop it first"),
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, experiment_key)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let previous =
        experiments::find_experiment(&state.pool, environment.id, &experiment_key).await?;
    experiments::delete_experiment(&state.pool, environment.id, &experiment_key).await?;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "experiment.deleted",
            "experiment",
            format!("{project_key}/{environment_key}/{experiment_key}"),
        )
        .in_environment(environment.id)
        .changing(Some(&previous), None),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/experiments/{experiment_key}/results",
    tag = "experiments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("experiment_key" = String, Path, description = "Experiment key"),
    ),
    responses((status = 200, description = "Counts, rates, intervals and verdicts per variant", body = ExperimentResults))
)]
pub async fn results(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, experiment_key)): Path<(String, String, String)>,
) -> ApiResult<Json<ExperimentResults>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let experiment =
        experiments::find_experiment(&state.pool, environment.id, &experiment_key).await?;
    let counts = experiments::experiment_counts(&state.pool, experiment.id).await?;

    let judged =
        flagforge_core::results(&experiment.control_variant, &in_flag_order(&experiment, counts));

    Ok(Json(ExperimentResults { experiment, results: judged }))
}

/// Orders counts by the flag's variant list, zero-filling arms that have no
/// counters yet and appending any counter variant the flag no longer defines.
fn in_flag_order(experiment: &Experiment, mut counts: Vec<VariantCounts>) -> Vec<VariantCounts> {
    let mut ordered = Vec::with_capacity(experiment.variants.len() + counts.len());
    for variant in &experiment.variants {
        match counts.iter().position(|c| c.variant == variant.key) {
            Some(index) => ordered.push(counts.swap_remove(index)),
            None => ordered.push(VariantCounts {
                variant: variant.key.clone(),
                exposures: 0,
                conversions: 0,
            }),
        }
    }
    counts.sort_by(|a, b| a.variant.cmp(&b.variant));
    ordered.extend(counts);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use flagforge_core::Variant;
    use uuid::Uuid;

    fn experiment() -> Experiment {
        Experiment {
            id: Uuid::nil(),
            environment_id: Uuid::nil(),
            flag_id: Uuid::nil(),
            flag_key: "checkout.v2".into(),
            variants: vec![Variant::new("on", true), Variant::new("off", false)],
            key: "checkout-cta".into(),
            name: "Checkout CTA".into(),
            description: None,
            metric_key: "order.completed".into(),
            control_variant: "off".into(),
            state: ExperimentState::Running,
            started_at: None,
            stopped_at: None,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn counts(variant: &str, exposures: u64) -> VariantCounts {
        VariantCounts { variant: variant.into(), exposures, conversions: 0 }
    }

    #[test]
    fn every_arm_renders_even_before_its_first_exposure() {
        let ordered = in_flag_order(&experiment(), vec![counts("off", 10)]);
        assert_eq!(
            ordered.iter().map(|c| (c.variant.as_str(), c.exposures)).collect::<Vec<_>>(),
            [("on", 0), ("off", 10)],
            "flag order, zero-filled"
        );
    }

    #[test]
    fn counters_for_a_renamed_variant_are_appended_not_hidden() {
        let ordered = in_flag_order(&experiment(), vec![counts("legacy", 7), counts("on", 3)]);
        assert_eq!(
            ordered.iter().map(|c| c.variant.as_str()).collect::<Vec<_>>(),
            ["on", "off", "legacy"]
        );
    }
}
