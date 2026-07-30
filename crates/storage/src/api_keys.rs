//! SDK keys: the credentials the evaluation endpoint authenticates.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Result, StorageError};
use crate::models::{ApiKey, KeyIdentity, KeyScope};

pub async fn create(
    pool: &PgPool,
    environment_id: Uuid,
    name: &str,
    key_hash: &str,
    prefix: &str,
    scope: KeyScope,
) -> Result<ApiKey> {
    let row = sqlx::query!(
        r#"
        INSERT INTO api_keys (environment_id, name, key_hash, prefix, scope)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, environment_id, name, prefix, scope, created_at, last_used_at, revoked_at
        "#,
        environment_id,
        name,
        key_hash,
        prefix,
        scope.as_str(),
    )
    .fetch_one(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "api key", name))?;

    Ok(ApiKey {
        id: row.id,
        environment_id: row.environment_id,
        name: row.name,
        prefix: row.prefix,
        scope: KeyScope::parse(&row.scope).unwrap_or(KeyScope::Client),
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        revoked_at: row.revoked_at,
    })
}

pub async fn list(pool: &PgPool, environment_id: Uuid) -> Result<Vec<ApiKey>> {
    let rows = sqlx::query!(
        r#"
        SELECT id, environment_id, name, prefix, scope, created_at, last_used_at, revoked_at
        FROM api_keys
        WHERE environment_id = $1
        ORDER BY created_at DESC
        "#,
        environment_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ApiKey {
            id: row.id,
            environment_id: row.environment_id,
            name: row.name,
            prefix: row.prefix,
            scope: KeyScope::parse(&row.scope).unwrap_or(KeyScope::Client),
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            revoked_at: row.revoked_at,
        })
        .collect())
}

/// Revokes a key. Idempotent by design — `revoked_at` is only set once, so a
/// retried revocation cannot move the timestamp of an already-dead key.
pub async fn revoke(pool: &PgPool, environment_id: Uuid, id: Uuid) -> Result<()> {
    let updated = sqlx::query!(
        r#"
        UPDATE api_keys
        SET revoked_at = COALESCE(revoked_at, now())
        WHERE id = $1 AND environment_id = $2
        "#,
        id,
        environment_id,
    )
    .execute(pool)
    .await?;

    if updated.rows_affected() == 0 {
        return Err(StorageError::not_found("api key"));
    }
    Ok(())
}

/// Resolves a presented key hash to the tenant it belongs to.
///
/// Revoked keys resolve to `None`, so revocation takes effect on the next
/// request rather than whenever a cache happens to expire.
pub async fn resolve(pool: &PgPool, key_hash: &str) -> Result<Option<KeyIdentity>> {
    let row = sqlx::query!(
        r#"
        SELECT k.id, k.environment_id, k.scope, e.project_id, p.organization_id
        FROM api_keys k
        JOIN environments e ON e.id = k.environment_id
        JOIN projects p     ON p.id = e.project_id
        WHERE k.key_hash = $1 AND k.revoked_at IS NULL
        "#,
        key_hash,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| KeyIdentity {
        api_key_id: r.id,
        environment_id: r.environment_id,
        project_id: r.project_id,
        organization_id: r.organization_id,
        scope: KeyScope::parse(&r.scope).unwrap_or(KeyScope::Client),
    }))
}

/// Records that a key was used.
///
/// Only moves the timestamp once a minute: SDKs poll constantly, and an
/// UPDATE per evaluation would turn a read-only hot path into a write storm.
pub async fn touch(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"
        UPDATE api_keys
        SET last_used_at = now()
        WHERE id = $1
          AND (last_used_at IS NULL OR last_used_at < now() - INTERVAL '1 minute')
        "#,
        id,
    )
    .execute(pool)
    .await?;
    Ok(())
}
