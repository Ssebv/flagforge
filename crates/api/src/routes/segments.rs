//! Reusable audiences.
//!
//! Segments are environment-scoped, so every route here sits under an
//! environment: "beta testers" in staging and in production are two different
//! sets of people, and the URL says so.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use flagforge_core::SegmentRule;
use flagforge_storage::models::{NewAuditEntry, Segment};
use flagforge_storage::{audit, projects, segments};
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::routes::valid_key;
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateSegment {
    #[schema(example = "beta-testers")]
    pub key: String,
    #[schema(example = "Beta testers")]
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateSegment {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    /// Context keys that are always members.
    pub included: Option<BTreeSet<String>>,
    /// Context keys that are never members; beats `included` and every rule.
    pub excluded: Option<BTreeSet<String>>,
    /// Membership rules. They are alternatives to each other — a context in
    /// any one of them is a member.
    pub rules: Option<Vec<SegmentRule>>,
    /// Version you last read. When present, the write is rejected if anything
    /// changed since. Worth sending: one segment edit moves every flag that
    /// references it.
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// A segment together with the flags currently pointing at it.
///
/// The dashboard needs the second half before it can offer a delete button
/// that will not fail, and an operator editing a shared audience should see
/// its blast radius on the same screen.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SegmentWithUsage {
    #[serde(flatten)]
    pub segment: Segment,
    pub referenced_by: Vec<String>,
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_key}/environments/{environment_key}/segments",
    tag = "segments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses((status = 200, description = "Segments in this environment", body = Vec<Segment>))
)]
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Segment>>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    Ok(Json(segments::list_segments(&state.pool, environment.id).await?))
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_key}/environments/{environment_key}/segments",
    tag = "segments", security(("bearer" = [])), request_body = CreateSegment,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
    ),
    responses(
        (status = 201, description = "Segment created", body = Segment),
        (status = 409, description = "A segment with that key already exists here"),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key)): Path<(String, String)>,
    Json(body): Json<CreateSegment>,
) -> ApiResult<(StatusCode, Json<Segment>)> {
    caller.require_write()?;
    valid_key(&body.key, "key")?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let segment = segments::create_segment(
        &state.pool,
        environment.id,
        &body.key,
        &body.name,
        body.description.as_deref(),
    )
    .await?;

    // A new segment has no members and no references, so nothing evaluates
    // differently yet — but the snapshot has to carry it before a flag rule is
    // allowed to name it.
    state.cache.refresh(environment.id).await;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "segment.created",
            "segment",
            format!("{project_key}/{environment_key}/{}", body.key),
        )
        .in_environment(environment.id)
        .changing(None, Some(&segment)),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(segment)))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/segments/{segment_key}",
    tag = "segments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("segment_key" = String, Path, description = "Segment key"),
    ),
    responses((status = 200, description = "The segment and what references it", body = SegmentWithUsage))
)]
pub async fn get(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, segment_key)): Path<(String, String, String)>,
) -> ApiResult<Json<SegmentWithUsage>> {
    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let segment = segments::find_segment(&state.pool, environment.id, &segment_key).await?;
    let referenced_by =
        segments::referencing_flags(&state.pool, environment.id, &segment_key).await?;

    Ok(Json(SegmentWithUsage { segment, referenced_by }))
}

#[utoipa::path(
    put,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/segments/{segment_key}",
    tag = "segments", security(("bearer" = [])), request_body = UpdateSegment,
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("segment_key" = String, Path, description = "Segment key"),
    ),
    responses(
        (status = 200, description = "Segment updated", body = Segment),
        (status = 409, description = "Someone else changed it first"),
        (status = 422, description = "The segment is not decidable"),
    )
)]
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, segment_key)): Path<(String, String, String)>,
    Json(body): Json<UpdateSegment>,
) -> ApiResult<Json<Segment>> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let previous = segments::find_segment(&state.pool, environment.id, &segment_key).await?;

    // Validate the segment as it will be *after* the patch, so a partial update
    // cannot arrive at a state a full write would have been refused.
    validate_decidable(&previous, &body)?;

    let segment = segments::update_segment(
        &state.pool,
        environment.id,
        &segment_key,
        body.name.as_deref(),
        body.description.as_ref().map(|d| d.as_deref()),
        body.included.as_ref(),
        body.excluded.as_ref(),
        body.rules.as_deref(),
        body.expected_version,
    )
    .await?;

    // The trigger has already notified every node; refreshing inline as well
    // means the caller's next read sees its own write rather than racing the
    // notification.
    state.cache.refresh(environment.id).await;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "segment.updated",
            "segment",
            format!("{project_key}/{environment_key}/{segment_key}"),
        )
        .in_environment(environment.id)
        .changing(Some(&previous), Some(&segment)),
    )
    .await?;

    tracing::info!(
        project = %project_key,
        environment = %environment_key,
        segment = %segment_key,
        version = segment.version,
        actor = %caller.email,
        "segment changed"
    );

    Ok(Json(segment))
}

#[utoipa::path(
    delete,
    path = "/api/v1/projects/{project_key}/environments/{environment_key}/segments/{segment_key}",
    tag = "segments", security(("bearer" = [])),
    params(
        ("project_key" = String, Path, description = "Project key"),
        ("environment_key" = String, Path, description = "Environment key"),
        ("segment_key" = String, Path, description = "Segment key"),
    ),
    responses(
        (status = 204, description = "Segment deleted"),
        (status = 409, description = "A flag rule still references it"),
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((project_key, environment_key, segment_key)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    caller.require_write()?;

    let project = projects::find_project(&state.pool, caller.organization_id, &project_key).await?;
    let environment = projects::find_environment(&state.pool, project.id, &environment_key).await?;

    let previous = segments::find_segment(&state.pool, environment.id, &segment_key).await?;
    segments::delete_segment(&state.pool, environment.id, &segment_key).await?;

    state.cache.refresh(environment.id).await;

    audit::record(
        &state.pool,
        NewAuditEntry::new(
            caller.organization_id,
            caller.actor(),
            "segment.deleted",
            "segment",
            format!("{project_key}/{environment_key}/{segment_key}"),
        )
        .in_environment(environment.id)
        .changing(Some(&previous), None),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Runs the domain validator over the segment the patch would produce.
fn validate_decidable(previous: &Segment, patch: &UpdateSegment) -> Result<(), ApiError> {
    let candidate = flagforge_core::Segment {
        key: previous.key.clone(),
        description: None,
        included: patch.included.clone().unwrap_or_else(|| previous.included.clone()),
        excluded: patch.excluded.clone().unwrap_or_else(|| previous.excluded.clone()),
        rules: patch.rules.clone().unwrap_or_else(|| previous.rules.clone()),
        version: previous.version,
    };

    flagforge_core::validate_segment(&candidate).map_err(ApiError::Unprocessable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use flagforge_core::SegmentRollout;
    use uuid::Uuid;

    fn stored() -> Segment {
        Segment {
            id: Uuid::nil(),
            environment_id: Uuid::nil(),
            key: "beta".into(),
            name: "Beta".into(),
            description: None,
            included: BTreeSet::from(["always-in".to_owned()]),
            excluded: BTreeSet::new(),
            rules: Vec::new(),
            version: 3,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn patch() -> UpdateSegment {
        UpdateSegment {
            name: None,
            description: None,
            included: None,
            excluded: None,
            rules: None,
            expected_version: None,
        }
    }

    #[test]
    fn an_empty_patch_leaves_a_valid_segment_valid() {
        assert!(validate_decidable(&stored(), &patch()).is_ok());
    }

    /// The check has to run against the merged result: excluding a key the
    /// *stored* segment includes is only a contradiction once the two halves
    /// are put together.
    #[test]
    fn a_patch_is_validated_against_the_state_it_would_produce() {
        let excluding =
            UpdateSegment { excluded: Some(BTreeSet::from(["always-in".to_owned()])), ..patch() };

        let error = validate_decidable(&stored(), &excluding).unwrap_err();
        let ApiError::Unprocessable(issues) = error else {
            panic!("expected a validation failure");
        };
        assert_eq!(issues[0].path, "excluded");
    }

    #[test]
    fn a_rollout_above_the_total_is_a_422() {
        let overshooting = UpdateSegment {
            rules: Some(vec![SegmentRule {
                rollout: Some(SegmentRollout {
                    percentage: flagforge_core::TOTAL_WEIGHT + 1,
                    bucket_by: None,
                }),
                ..SegmentRule::new(Uuid::nil(), vec![])
            }]),
            ..patch()
        };

        assert!(matches!(
            validate_decidable(&stored(), &overshooting),
            Err(ApiError::Unprocessable(_))
        ));
    }
}
