//! Building an in-memory [`EnvironmentSnapshot`] from the database.

use chrono::Utc;
use flagforge_core::{EnvironmentSnapshot, Flag};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{FoundExt, Result, StorageError};
use crate::flags::load_environment_flags;

/// Loads everything one environment needs to answer evaluations.
///
/// Two queries: the environment (for its salt) and every configured flag. The
/// result is self-contained, so once it is in memory the evaluation path never
/// touches Postgres again.
pub async fn load(pool: &PgPool, environment_id: Uuid) -> Result<EnvironmentSnapshot> {
    let env = sqlx::query!(r#"SELECT key, salt FROM environments WHERE id = $1"#, environment_id,)
        .fetch_optional(pool)
        .await?
        .or_not_found("environment")?;

    let rows = load_environment_flags(pool, environment_id).await?;

    let mut flags = Vec::with_capacity(rows.len());
    for row in rows {
        flags.push(Flag {
            key: row.key,
            variants: serde_json::from_value(row.variants)
                .map_err(|e| StorageError::malformed("flag variants", e))?,
            enabled: row.enabled,
            off_variant: row.off_variant,
            fallthrough: serde_json::from_value(row.fallthrough)
                .map_err(|e| StorageError::malformed("flag fallthrough", e))?,
            rules: serde_json::from_value(row.rules)
                .map_err(|e| StorageError::malformed("flag rules", e))?,
            version: row.version,
        });
    }

    Ok(EnvironmentSnapshot::new(environment_id, env.key, env.salt, flags, Utc::now()))
}

/// Resolves `org / project-key / environment-key` to an environment id.
///
/// Scoped by organization so a caller cannot walk into another tenant by
/// guessing project names.
pub async fn resolve_environment(
    pool: &PgPool,
    organization_id: Uuid,
    project_key: &str,
    environment_key: &str,
) -> Result<Uuid> {
    let row = sqlx::query!(
        r#"
        SELECT e.id
        FROM environments e
        JOIN projects p ON p.id = e.project_id
        WHERE p.organization_id = $1 AND p.key = $2 AND e.key = $3
        "#,
        organization_id,
        project_key,
        environment_key,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("environment")?;

    Ok(row.id)
}
