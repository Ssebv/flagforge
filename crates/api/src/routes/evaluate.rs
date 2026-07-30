//! The SDK-facing evaluation endpoints.
//!
//! This is the hot path. Everything it needs is already in memory, so a
//! request here does no database work at all — authentication resolves a key,
//! and the decision itself is a pure function over a cached snapshot.

use axum::Json;
use axum::extract::{Path, State};
use flagforge_core::{Evaluation, EvaluationContext, VariantValue};
use serde::{Deserialize, Serialize};

use crate::auth::SdkIdentity;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Refuses absurd payloads before they become work.
const MAX_REQUESTED_FLAGS: usize = 1_000;
const MAX_CONTEXT_ATTRIBUTES: usize = 100;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct EvaluateRequest {
    pub context: EvaluationContext,
    /// Restrict the response to these flags. Omit for all of them.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EvaluateResponse {
    pub environment: String,
    /// Snapshot version the decisions were made against.
    pub version: i64,
    pub evaluations: Vec<Evaluation>,
}

/// Evaluates every flag in the environment (or a named subset).
///
/// This is what an SDK calls once at startup so it can answer locally
/// afterwards.
#[utoipa::path(
    post, path = "/api/v1/evaluate", tag = "evaluate",
    security(("sdk_key" = [])), request_body = EvaluateRequest,
    responses(
        (status = 200, description = "Decisions for the caller's environment", body = EvaluateResponse),
        (status = 400, description = "The context is too large"),
        (status = 401, description = "Missing, unknown or revoked SDK key"),
        (status = 429, description = "Rate limited"),
    )
)]
pub async fn evaluate_all(
    State(state): State<AppState>,
    identity: SdkIdentity,
    Json(body): Json<EvaluateRequest>,
) -> ApiResult<Json<EvaluateResponse>> {
    check_context(&body.context)?;

    if let Some(requested) = &body.flags
        && requested.len() > MAX_REQUESTED_FLAGS
    {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_REQUESTED_FLAGS} flags can be requested at once"
        )));
    }

    let snapshot = state.cache.get(identity.environment_id()).await?;

    let evaluations = match &body.flags {
        None => snapshot.evaluate_all(&body.context),
        Some(requested) => requested
            .iter()
            .map(|key| snapshot.evaluate(key, &body.context, VariantValue::null()))
            .collect(),
    };

    record_metrics(&evaluations);

    Ok(Json(EvaluateResponse {
        environment: snapshot.environment_key.clone(),
        version: snapshot.version,
        evaluations,
    }))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct EvaluateOneRequest {
    pub context: EvaluationContext,
    /// Returned when the flag does not exist here. Lets a caller keep its own
    /// fallback in one place instead of duplicating it around every call site.
    #[serde(default)]
    pub default: Option<VariantValue>,
}

/// Evaluates a single flag.
#[utoipa::path(
    post, path = "/api/v1/evaluate/{flag_key}", tag = "evaluate",
    security(("sdk_key" = [])), request_body = EvaluateOneRequest,
    params(("flag_key" = String, Path, description = "Flag key")),
    responses(
        (status = 200, description = "The decision, with the reason behind it", body = Evaluation),
        (status = 401, description = "Missing, unknown or revoked SDK key"),
    )
)]
pub async fn evaluate_one(
    State(state): State<AppState>,
    identity: SdkIdentity,
    Path(flag_key): Path<String>,
    Json(body): Json<EvaluateOneRequest>,
) -> ApiResult<Json<Evaluation>> {
    check_context(&body.context)?;

    let snapshot = state.cache.get(identity.environment_id()).await?;
    let fallback = body.default.unwrap_or_else(VariantValue::null);
    let evaluation = snapshot.evaluate(&flag_key, &body.context, fallback);

    record_metrics(std::slice::from_ref(&evaluation));

    Ok(Json(evaluation))
}

/// The whole environment configuration, for SDKs that evaluate locally.
///
/// Shipping the rules rather than the decisions is what lets a client-side SDK
/// answer without a network round trip per flag. The bucketing salt is
/// deliberately excluded from the payload.
#[utoipa::path(
    get, path = "/api/v1/snapshot", tag = "evaluate", security(("sdk_key" = [])),
    responses(
        (status = 200, description = "Flag configuration for the caller's environment"),
        (status = 401, description = "Missing, unknown or revoked SDK key"),
        (status = 403, description = "Client-scoped keys cannot download rules"),
    )
)]
pub async fn snapshot(
    State(state): State<AppState>,
    identity: SdkIdentity,
) -> ApiResult<Json<flagforge_core::EnvironmentSnapshot>> {
    // Targeting rules name attributes and segments — "employees", "beta
    // customers", specific emails. That is internal information, so a key that
    // ships to browsers only gets decisions, never the rules behind them.
    if identity.scope() == flagforge_storage::models::KeyScope::Client {
        return Err(ApiError::Forbidden(
            "client-scoped keys can evaluate but cannot download targeting rules",
        ));
    }

    let snapshot = state.cache.get(identity.environment_id()).await?;
    Ok(Json((*snapshot).clone()))
}

fn check_context(context: &EvaluationContext) -> Result<(), ApiError> {
    if context.key.is_empty() {
        return Err(ApiError::BadRequest(
            "context.key is required; it is what percentage rollouts bucket on".into(),
        ));
    }
    if context.attributes.len() > MAX_CONTEXT_ATTRIBUTES {
        return Err(ApiError::BadRequest(format!(
            "a context may carry at most {MAX_CONTEXT_ATTRIBUTES} attributes"
        )));
    }
    Ok(())
}

/// Counts decisions by reason.
///
/// The `error` and `flag_not_found` series are the useful ones: they are how
/// you notice an SDK asking for a flag nobody ever created.
fn record_metrics(evaluations: &[Evaluation]) {
    for evaluation in evaluations {
        let reason = match &evaluation.reason {
            flagforge_core::Reason::Off => "off",
            flagforge_core::Reason::TargetMatch { .. } => "target_match",
            flagforge_core::Reason::Fallthrough => "fallthrough",
            flagforge_core::Reason::FlagNotFound => "flag_not_found",
            flagforge_core::Reason::Error { .. } => "error",
        };
        metrics::counter!("flagforge_evaluations_total", "reason" => reason).increment(1);

        if !evaluation.reason.is_healthy() {
            tracing::warn!(
                flag = %evaluation.flag_key,
                reason = ?evaluation.reason,
                "degraded flag evaluation"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_context_without_a_key_is_rejected() {
        let error = check_context(&EvaluationContext::new("")).unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    #[test]
    fn an_oversized_context_is_rejected() {
        let mut context = EvaluationContext::new("u");
        for i in 0..=MAX_CONTEXT_ATTRIBUTES {
            context.attributes.insert(format!("attr-{i}"), (i as i64).into());
        }
        assert!(check_context(&context).is_err());
    }

    #[test]
    fn an_ordinary_context_passes() {
        assert!(check_context(&EvaluationContext::new("user-1").with("plan", "pro")).is_ok());
    }
}
