//! The audit trail.
//!
//! "Who turned this on in production, and when?" is the first question asked
//! during an incident. Recording it is not optional bookkeeping — it is the
//! feature that makes a flag service safe to give to a whole company.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;
use crate::models::{AuditEntry, NewAuditEntry};

/// Maximum page size, so a client cannot ask for the whole history at once.
pub const MAX_PAGE_SIZE: i64 = 200;

pub async fn record(pool: &PgPool, entry: NewAuditEntry) -> Result<i64> {
    let row = sqlx::query!(
        r#"
        INSERT INTO audit_log (
            organization_id, actor_user_id, actor_email, action,
            resource_type, resource_id, environment_id, before, after
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id
        "#,
        entry.organization_id,
        entry.actor_user_id,
        entry.actor_email,
        entry.action,
        entry.resource_type,
        entry.resource_id,
        entry.environment_id,
        entry.before,
        entry.after,
    )
    .fetch_one(pool)
    .await?;

    Ok(row.id)
}

/// Keyset pagination: pass the last id you saw as `before_id`.
///
/// Offsets would drift as new entries land at the head of a log that is
/// written to constantly; a cursor on the primary key does not.
pub async fn list(
    pool: &PgPool,
    organization_id: Uuid,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<AuditEntry>> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);

    let rows = sqlx::query!(
        r#"
        SELECT id, actor_email, action, resource_type, resource_id,
               environment_id, before, after, created_at
        FROM audit_log
        WHERE organization_id = $1
          AND ($2::TEXT   IS NULL OR resource_type = $2)
          AND ($3::TEXT   IS NULL OR resource_id   = $3)
          AND ($4::BIGINT IS NULL OR id            < $4)
        ORDER BY id DESC
        LIMIT $5
        "#,
        organization_id,
        resource_type,
        resource_id,
        before_id,
        limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AuditEntry {
            id: r.id,
            actor_email: r.actor_email,
            action: r.action,
            resource_type: r.resource_type,
            resource_id: r.resource_id,
            environment_id: r.environment_id,
            before: r.before,
            after: r.after,
            created_at: r.created_at,
        })
        .collect())
}
