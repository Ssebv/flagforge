//! Flag definitions and their per-environment configuration.

use chrono::{DateTime, Utc};
use flagforge_core::{Distribution, Rule, Variant};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{FoundExt, Result, StorageError};
use crate::models::{Flag, FlagConfig};

/// Creates a flag and seeds it, disabled, into every environment of the
/// project.
///
/// Seeding matters: a flag that exists in the API but not in an environment
/// would make SDKs fall back to their hard-coded default with no way to tell
/// that from "the flag is off", which is exactly the ambiguity flags exist to
/// remove.
#[allow(clippy::too_many_arguments)]
pub async fn create_flag(
    pool: &PgPool,
    project_id: Uuid,
    key: &str,
    name: &str,
    description: Option<&str>,
    variants: &[Variant],
    off_variant: &str,
    fallthrough: &Distribution,
) -> Result<Flag> {
    let variants_json =
        serde_json::to_value(variants).map_err(|e| StorageError::malformed("flag", e))?;
    let fallthrough_json =
        serde_json::to_value(fallthrough).map_err(|e| StorageError::malformed("flag", e))?;

    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    let row = sqlx::query!(
        r#"
        INSERT INTO flags (project_id, key, name, description, variants)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, project_id, key, name, description, variants, archived, created_at, updated_at
        "#,
        project_id,
        key,
        name,
        description,
        variants_json,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| StorageError::from_write(e, "flag", key))?;

    sqlx::query!(
        r#"
        INSERT INTO flag_configs (flag_id, environment_id, enabled, off_variant, fallthrough, rules)
        SELECT $1, e.id, FALSE, $2, $3, '[]'::JSONB
        FROM environments e
        WHERE e.project_id = $4
        "#,
        row.id,
        off_variant,
        fallthrough_json,
        project_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Flag {
        id: row.id,
        project_id: row.project_id,
        key: row.key,
        name: row.name,
        description: row.description,
        variants: decode_variants(row.variants)?,
        archived: row.archived,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub async fn list_flags(
    pool: &PgPool,
    project_id: Uuid,
    include_archived: bool,
) -> Result<Vec<Flag>> {
    let rows = sqlx::query!(
        r#"
        SELECT id, project_id, key, name, description, variants, archived, created_at, updated_at
        FROM flags
        WHERE project_id = $1 AND ($2 OR NOT archived)
        ORDER BY key
        "#,
        project_id,
        include_archived,
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(Flag {
                id: row.id,
                project_id: row.project_id,
                key: row.key,
                name: row.name,
                description: row.description,
                variants: decode_variants(row.variants)?,
                archived: row.archived,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
        .collect()
}

pub async fn find_flag(pool: &PgPool, project_id: Uuid, key: &str) -> Result<Flag> {
    let row = sqlx::query!(
        r#"
        SELECT id, project_id, key, name, description, variants, archived, created_at, updated_at
        FROM flags
        WHERE project_id = $1 AND key = $2
        "#,
        project_id,
        key,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("flag")?;

    Ok(Flag {
        id: row.id,
        project_id: row.project_id,
        key: row.key,
        name: row.name,
        description: row.description,
        variants: decode_variants(row.variants)?,
        archived: row.archived,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Partial update; `None` leaves a column untouched.
pub async fn update_flag(
    pool: &PgPool,
    flag_id: Uuid,
    name: Option<&str>,
    description: Option<Option<&str>>,
    variants: Option<&[Variant]>,
    archived: Option<bool>,
) -> Result<Flag> {
    let variants_json = match variants {
        Some(v) => Some(serde_json::to_value(v).map_err(|e| StorageError::malformed("flag", e))?),
        None => None,
    };
    // `description` is nullable, so "leave alone" and "set to NULL" have to be
    // distinguishable — hence the flag column alongside the value.
    let (clear_description, description_value) = match description {
        None => (false, None),
        Some(value) => (true, value),
    };

    let row = sqlx::query!(
        r#"
        UPDATE flags
        SET name        = COALESCE($2, name),
            description = CASE WHEN $3 THEN $4 ELSE description END,
            variants    = COALESCE($5, variants),
            archived    = COALESCE($6, archived)
        WHERE id = $1
        RETURNING id, project_id, key, name, description, variants, archived, created_at, updated_at
        "#,
        flag_id,
        name,
        clear_description,
        description_value,
        variants_json,
        archived,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "flag", flag_id.to_string()))?
    .or_not_found("flag")?;

    Ok(Flag {
        id: row.id,
        project_id: row.project_id,
        key: row.key,
        name: row.name,
        description: row.description,
        variants: decode_variants(row.variants)?,
        archived: row.archived,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub async fn delete_flag(pool: &PgPool, project_id: Uuid, key: &str) -> Result<()> {
    let deleted =
        sqlx::query!(r#"DELETE FROM flags WHERE project_id = $1 AND key = $2"#, project_id, key)
            .execute(pool)
            .await?;

    if deleted.rows_affected() == 0 {
        return Err(StorageError::not_found("flag"));
    }
    Ok(())
}

pub async fn find_config(pool: &PgPool, flag_id: Uuid, environment_id: Uuid) -> Result<FlagConfig> {
    let row = sqlx::query!(
        r#"
        SELECT flag_id, environment_id, enabled, off_variant, fallthrough, rules, version, updated_at
        FROM flag_configs
        WHERE flag_id = $1 AND environment_id = $2
        "#,
        flag_id,
        environment_id,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("flag configuration")?;

    build_config(
        row.flag_id,
        row.environment_id,
        row.enabled,
        row.off_variant,
        row.fallthrough,
        row.rules,
        row.version,
        row.updated_at,
    )
}

/// Writes a flag's configuration for one environment.
///
/// `expected_version` implements optimistic concurrency: pass the version you
/// read and the write is rejected if someone else changed the flag meanwhile.
/// Two operators editing production at once is exactly when silently
/// overwriting each other is least acceptable.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_config(
    pool: &PgPool,
    flag_id: Uuid,
    environment_id: Uuid,
    enabled: bool,
    off_variant: &str,
    fallthrough: &Distribution,
    rules: &[Rule],
    expected_version: Option<i64>,
) -> Result<FlagConfig> {
    let fallthrough_json = serde_json::to_value(fallthrough)
        .map_err(|e| StorageError::malformed("flag configuration", e))?;
    let rules_json = serde_json::to_value(rules)
        .map_err(|e| StorageError::malformed("flag configuration", e))?;

    let row = sqlx::query!(
        r#"
        INSERT INTO flag_configs (flag_id, environment_id, enabled, off_variant, fallthrough, rules)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (flag_id, environment_id) DO UPDATE
        SET enabled     = EXCLUDED.enabled,
            off_variant = EXCLUDED.off_variant,
            fallthrough = EXCLUDED.fallthrough,
            rules       = EXCLUDED.rules
        WHERE $7::BIGINT IS NULL OR flag_configs.version = $7
        RETURNING flag_id, environment_id, enabled, off_variant, fallthrough, rules, version, updated_at
        "#,
        flag_id,
        environment_id,
        enabled,
        off_variant,
        fallthrough_json,
        rules_json,
        expected_version,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "flag configuration", flag_id.to_string()))?;

    // No row came back: the ON CONFLICT branch was filtered out by the version
    // guard, i.e. someone else wrote first.
    let row = row.ok_or(StorageError::VersionConflict {
        entity: "flag configuration",
        expected: expected_version.unwrap_or_default(),
    })?;

    build_config(
        row.flag_id,
        row.environment_id,
        row.enabled,
        row.off_variant,
        row.fallthrough,
        row.rules,
        row.version,
        row.updated_at,
    )
}

/// Every flag configuration in one environment, joined with its definition.
///
/// This is the query behind snapshot loading: one index scan, one round trip,
/// no N+1.
pub(crate) struct ConfiguredFlagRow {
    pub key: String,
    pub variants: serde_json::Value,
    pub enabled: bool,
    pub off_variant: String,
    pub fallthrough: serde_json::Value,
    pub rules: serde_json::Value,
    pub version: i64,
}

pub(crate) async fn load_environment_flags(
    pool: &PgPool,
    environment_id: Uuid,
) -> Result<Vec<ConfiguredFlagRow>> {
    let rows = sqlx::query!(
        r#"
        SELECT f.key, f.variants, c.enabled, c.off_variant, c.fallthrough, c.rules, c.version
        FROM flag_configs c
        JOIN flags f ON f.id = c.flag_id
        WHERE c.environment_id = $1 AND NOT f.archived
        ORDER BY f.key
        "#,
        environment_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ConfiguredFlagRow {
            key: r.key,
            variants: r.variants,
            enabled: r.enabled,
            off_variant: r.off_variant,
            fallthrough: r.fallthrough,
            rules: r.rules,
            version: r.version,
        })
        .collect())
}

fn decode_variants(raw: serde_json::Value) -> Result<Vec<Variant>> {
    serde_json::from_value(raw).map_err(|e| StorageError::malformed("flag", e))
}

#[allow(clippy::too_many_arguments)]
fn build_config(
    flag_id: Uuid,
    environment_id: Uuid,
    enabled: bool,
    off_variant: String,
    fallthrough: serde_json::Value,
    rules: serde_json::Value,
    version: i64,
    updated_at: DateTime<Utc>,
) -> Result<FlagConfig> {
    Ok(FlagConfig {
        flag_id,
        environment_id,
        enabled,
        off_variant,
        fallthrough: serde_json::from_value(fallthrough)
            .map_err(|e| StorageError::malformed("flag configuration", e))?,
        rules: serde_json::from_value(rules)
            .map_err(|e| StorageError::malformed("flag configuration", e))?,
        version,
        updated_at,
    })
}
