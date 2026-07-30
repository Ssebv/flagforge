//! Reading the audit trail.

use axum::Json;
use axum::extract::{Query, State};
use flagforge_storage::audit;
use flagforge_storage::models::AuditEntry;
use serde::{Deserialize, Serialize};

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

const DEFAULT_PAGE_SIZE: i64 = 50;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AuditQuery {
    /// Filter by resource type, e.g. `flag`.
    pub resource_type: Option<String>,
    /// Filter by resource id, e.g. `checkout/production/checkout.v2`.
    pub resource_id: Option<String>,
    /// Cursor: return entries older than this id.
    pub before_id: Option<i64>,
    /// 1-200, defaults to 50.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    /// Pass as `before_id` to fetch the next page; absent on the last page.
    pub next_cursor: Option<i64>,
}

/// Returns the organization's change history, newest first.
#[utoipa::path(
    get, path = "/api/v1/audit", tag = "audit",
    security(("bearer" = [])), params(AuditQuery),
    responses(
        (status = 200, description = "A page of audit entries", body = AuditPage),
        (status = 403, description = "Requires an owner or admin"),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Query(query): Query<AuditQuery>,
) -> ApiResult<Json<AuditPage>> {
    // The trail records who did what, which is exactly the kind of thing that
    // should not be readable by every member of the organization.
    caller.require_admin()?;

    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);

    let entries = audit::list(
        &state.pool,
        caller.organization_id,
        query.resource_type.as_deref(),
        query.resource_id.as_deref(),
        query.before_id,
        limit,
    )
    .await?;

    // Only advertise a cursor when the page came back full; a short page is
    // already the end of the log.
    let next_cursor = (entries.len() as i64 >= limit.clamp(1, audit::MAX_PAGE_SIZE))
        .then(|| entries.last().map(|e| e.id))
        .flatten();

    Ok(Json(AuditPage { entries, next_cursor }))
}
