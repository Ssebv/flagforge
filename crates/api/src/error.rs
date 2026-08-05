//! API errors and their wire representation.
//!
//! Responses follow RFC 9457 (`application/problem+json`), so a client gets a
//! machine-readable `type` and a human-readable `detail` instead of having to
//! pattern-match on prose.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use flagforge_core::ValidationIssue;
use flagforge_storage::StorageError;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Unauthorized(&'static str),

    #[error("{0}")]
    Forbidden(&'static str),

    #[error("{0} not found")]
    NotFound(&'static str),

    #[error("{0}")]
    Conflict(String),

    /// The request parsed but the configuration it describes is not evaluable.
    #[error("the submitted configuration is invalid")]
    Unprocessable(Vec<ValidationIssue>),

    #[error("too many requests")]
    RateLimited { retry_after_secs: u64 },

    /// Anything we did not anticipate. The cause is logged, never returned.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable slug clients can branch on, independent of wording changes.
    fn kind(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized(_) => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Unprocessable(_) => "validation_failed",
            Self::RateLimited { .. } => "rate_limited",
            Self::Internal(_) => "internal_error",
        }
    }
}

/// The body of every error response.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProblemDetails {
    /// Stable error slug, e.g. `validation_failed`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub title: String,
    pub status: u16,
    /// Field-level problems, present for `validation_failed`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ValidationIssue>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let kind = self.kind();

        // Internal errors are logged in full and reported as a bare 500: the
        // cause frequently contains connection strings or SQL.
        let title = match &self {
            Self::Internal(cause) => {
                tracing::error!(error = ?cause, "unhandled error while serving a request");
                "an internal error occurred".to_owned()
            }
            other => other.to_string(),
        };

        let errors = match &self {
            Self::Unprocessable(issues) => issues.clone(),
            _ => Vec::new(),
        };

        metrics::counter!("flagforge_api_errors_total", "kind" => kind).increment(1);

        let mut response =
            (status, Json(ProblemDetails { kind, title, status: status.as_u16(), errors }))
                .into_response();

        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );

        if let Self::RateLimited { retry_after_secs } = self
            && let Ok(value) = axum::http::HeaderValue::from_str(&retry_after_secs.to_string())
        {
            response.headers_mut().insert(axum::http::header::RETRY_AFTER, value);
        }

        response
    }
}

impl From<StorageError> for ApiError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::NotFound { entity } => Self::NotFound(entity),
            StorageError::Conflict { entity, key } => {
                Self::Conflict(format!("{entity} `{key}` already exists"))
            }
            // Same status, different meaning: nothing is duplicated, the
            // caller simply lost a race and should re-read before retrying.
            conflict @ StorageError::VersionConflict { .. } => Self::Conflict(conflict.to_string()),
            // Also a 409: the request is well formed and the row exists, but
            // honouring it would leave a rule pointing at nothing. The message
            // names what has to be changed first.
            in_use @ StorageError::InUse { .. } => Self::Conflict(in_use.to_string()),
            StorageError::Invalid { entity } => {
                Self::BadRequest(format!("{entity} failed a database constraint"))
            }
            // Both of these mean the server is broken, not the request.
            other @ (StorageError::Malformed { .. } | StorageError::Database(_)) => {
                Self::Internal(anyhow::Error::new(other))
            }
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::BadRequest(rejection.body_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(error: ApiError) -> (StatusCode, String, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, content_type, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn problems_are_served_as_problem_json() {
        let (status, content_type, body) = body_of(ApiError::NotFound("project")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type, "application/problem+json");
        assert_eq!(body["type"], "not_found");
        assert_eq!(body["title"], "project not found");
    }

    #[tokio::test]
    async fn internal_errors_never_leak_their_cause() {
        let secret = "postgres://user:hunter2@db/flagforge";
        let (status, _, body) =
            body_of(ApiError::Internal(anyhow::anyhow!("connect failed: {secret}"))).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!body.to_string().contains("hunter2"), "{body}");
        assert_eq!(body["title"], "an internal error occurred");
    }

    #[tokio::test]
    async fn validation_failures_carry_field_paths() {
        let issues = vec![ValidationIssue {
            path: "fallthrough.weights".into(),
            message: "weights must sum to 100000".into(),
        }];
        let (status, _, body) = body_of(ApiError::Unprocessable(issues)).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["errors"][0]["path"], "fallthrough.weights");
    }

    #[tokio::test]
    async fn rate_limiting_tells_the_client_when_to_retry() {
        let response = ApiError::RateLimited { retry_after_secs: 3 }.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[axum::http::header::RETRY_AFTER], "3");
    }

    #[test]
    fn storage_not_found_maps_to_404_not_500() {
        let mapped: ApiError = StorageError::not_found("flag").into();
        assert_eq!(mapped.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_lost_write_race_explains_itself() {
        let mapped: ApiError =
            StorageError::VersionConflict { entity: "flag configuration", expected: 4 }.into();

        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        let message = mapped.to_string();
        assert!(message.contains("modified by someone else"), "{message}");
        assert!(message.contains("version 4"), "{message}");
        // "already exists" would send a user hunting for a duplicate that is
        // not there.
        assert!(!message.contains("already exists"), "{message}");
    }

    #[test]
    fn a_malformed_stored_row_is_a_500_not_a_client_error() {
        let broken = serde_json::from_str::<i32>("not json").unwrap_err();
        let mapped: ApiError = StorageError::malformed("flag", broken).into();
        assert_eq!(mapped.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
