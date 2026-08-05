//! Reusable audiences, scoped to an environment.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use flagforge_core::SegmentRule;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{FoundExt, Result, StorageError};
use crate::models::Segment;

pub async fn create_segment(
    pool: &PgPool,
    environment_id: Uuid,
    key: &str,
    name: &str,
    description: Option<&str>,
) -> Result<Segment> {
    let row = sqlx::query!(
        r#"
        INSERT INTO segments (environment_id, key, name, description)
        VALUES ($1, $2, $3, $4)
        RETURNING id, environment_id, key, name, description, included, excluded, rules,
                  version, created_at, updated_at
        "#,
        environment_id,
        key,
        name,
        description,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "segment", key))?;

    build(
        row.id,
        row.environment_id,
        row.key,
        row.name,
        row.description,
        row.included,
        row.excluded,
        row.rules,
        row.version,
        row.created_at,
        row.updated_at,
    )
}

pub async fn list_segments(pool: &PgPool, environment_id: Uuid) -> Result<Vec<Segment>> {
    let rows = sqlx::query!(
        r#"
        SELECT id, environment_id, key, name, description, included, excluded, rules,
               version, created_at, updated_at
        FROM segments
        WHERE environment_id = $1
        ORDER BY key
        "#,
        environment_id,
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            build(
                row.id,
                row.environment_id,
                row.key,
                row.name,
                row.description,
                row.included,
                row.excluded,
                row.rules,
                row.version,
                row.created_at,
                row.updated_at,
            )
        })
        .collect()
}

pub async fn find_segment(pool: &PgPool, environment_id: Uuid, key: &str) -> Result<Segment> {
    let row = sqlx::query!(
        r#"
        SELECT id, environment_id, key, name, description, included, excluded, rules,
               version, created_at, updated_at
        FROM segments
        WHERE environment_id = $1 AND key = $2
        "#,
        environment_id,
        key,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("segment")?;

    build(
        row.id,
        row.environment_id,
        row.key,
        row.name,
        row.description,
        row.included,
        row.excluded,
        row.rules,
        row.version,
        row.created_at,
        row.updated_at,
    )
}

/// Rewrites a segment's membership definition.
///
/// `expected_version` is the same optimistic-concurrency guard the flag
/// configuration write uses, and matters more here: one segment edit changes
/// what every referencing flag serves, so a silently lost update is a wider
/// blast radius than a lost flag edit.
#[allow(clippy::too_many_arguments)]
pub async fn update_segment(
    pool: &PgPool,
    environment_id: Uuid,
    key: &str,
    name: Option<&str>,
    description: Option<Option<&str>>,
    included: Option<&BTreeSet<String>>,
    excluded: Option<&BTreeSet<String>>,
    rules: Option<&[SegmentRule]>,
    expected_version: Option<i64>,
) -> Result<Segment> {
    let included_json = match included {
        Some(v) => {
            Some(serde_json::to_value(v).map_err(|e| StorageError::malformed("segment", e))?)
        }
        None => None,
    };
    let excluded_json = match excluded {
        Some(v) => {
            Some(serde_json::to_value(v).map_err(|e| StorageError::malformed("segment", e))?)
        }
        None => None,
    };
    let rules_json = match rules {
        Some(v) => {
            Some(serde_json::to_value(v).map_err(|e| StorageError::malformed("segment", e))?)
        }
        None => None,
    };
    // `description` is nullable, so "leave alone" and "set to NULL" have to be
    // distinguishable — the same pattern `update_flag` uses.
    let (clear_description, description_value) = match description {
        None => (false, None),
        Some(value) => (true, value),
    };

    // Split into two steps so a version mismatch can be told apart from a
    // missing segment: a single UPDATE returning no rows cannot distinguish
    // them, and reporting "not found" for a concurrent edit would send the
    // operator looking in the wrong place.
    let current = find_segment(pool, environment_id, key).await?;
    if let Some(expected) = expected_version
        && current.version != expected
    {
        return Err(StorageError::VersionConflict { entity: "segment", expected });
    }

    let row = sqlx::query!(
        r#"
        UPDATE segments
        SET name        = COALESCE($3, name),
            description = CASE WHEN $4 THEN $5 ELSE description END,
            included    = COALESCE($6, included),
            excluded    = COALESCE($7, excluded),
            rules       = COALESCE($8, rules)
        WHERE environment_id = $1 AND key = $2 AND ($9::BIGINT IS NULL OR version = $9)
        RETURNING id, environment_id, key, name, description, included, excluded, rules,
                  version, created_at, updated_at
        "#,
        environment_id,
        key,
        name,
        clear_description,
        description_value,
        included_json,
        excluded_json,
        rules_json,
        expected_version,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "segment", key))?
    // The row was there a moment ago and the version matched, so losing the
    // race here means someone else wrote in between.
    .ok_or(StorageError::VersionConflict {
        entity: "segment",
        expected: expected_version.unwrap_or(current.version),
    })?;

    build(
        row.id,
        row.environment_id,
        row.key,
        row.name,
        row.description,
        row.included,
        row.excluded,
        row.rules,
        row.version,
        row.created_at,
        row.updated_at,
    )
}

/// Flag keys whose rules reference `segment_key` in this environment.
///
/// Postgres cannot express this as a foreign key — the reference lives inside a
/// JSONB rule — so it is checked here, and the query does the containment test
/// rather than pulling every rule set into the process.
pub async fn referencing_flags(
    pool: &PgPool,
    environment_id: Uuid,
    segment_key: &str,
) -> Result<Vec<String>> {
    let any_of = serde_json::json!([{"segments": {"any_of": [segment_key]}}]);
    let none_of = serde_json::json!([{"segments": {"none_of": [segment_key]}}]);

    let rows = sqlx::query!(
        r#"
        SELECT f.key
        FROM flag_configs c
        JOIN flags f ON f.id = c.flag_id
        WHERE c.environment_id = $1 AND (c.rules @> $2::JSONB OR c.rules @> $3::JSONB)
        ORDER BY f.key
        "#,
        environment_id,
        any_of,
        none_of,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| r.key).collect())
}

/// Deletes a segment, refusing while any flag rule still references it.
///
/// The engine already fails closed on a dangling reference, but silently
/// turning a targeted rule into one that matches nobody is a bad way to find
/// out. Refusing turns it into an error the operator reads before anything
/// changes.
pub async fn delete_segment(pool: &PgPool, environment_id: Uuid, key: &str) -> Result<()> {
    let referencing = referencing_flags(pool, environment_id, key).await?;
    if !referencing.is_empty() {
        return Err(StorageError::InUse {
            entity: "segment",
            key: key.to_owned(),
            referenced_by: referencing,
        });
    }

    let deleted = sqlx::query!(
        r#"DELETE FROM segments WHERE environment_id = $1 AND key = $2"#,
        environment_id,
        key,
    )
    .execute(pool)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(StorageError::not_found("segment"));
    }
    Ok(())
}

/// Just the keys defined in one environment.
///
/// The flag write path needs to know which references are valid, not what the
/// segments contain — and decoding every rule set to answer that would be work
/// thrown away on every configuration write.
pub async fn segment_keys(pool: &PgPool, environment_id: Uuid) -> Result<BTreeSet<String>> {
    let rows =
        sqlx::query!(r#"SELECT key FROM segments WHERE environment_id = $1"#, environment_id)
            .fetch_all(pool)
            .await?;

    Ok(rows.into_iter().map(|r| r.key).collect())
}

/// Every segment in one environment, for snapshot loading.
pub(crate) async fn load_environment_segments(
    pool: &PgPool,
    environment_id: Uuid,
) -> Result<Vec<flagforge_core::Segment>> {
    Ok(list_segments(pool, environment_id).await?.iter().map(Segment::definition).collect())
}

#[allow(clippy::too_many_arguments)]
fn build(
    id: Uuid,
    environment_id: Uuid,
    key: String,
    name: String,
    description: Option<String>,
    included: serde_json::Value,
    excluded: serde_json::Value,
    rules: serde_json::Value,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> Result<Segment> {
    Ok(Segment {
        id,
        environment_id,
        key,
        name,
        description,
        included: serde_json::from_value(included)
            .map_err(|e| StorageError::malformed("segment", e))?,
        excluded: serde_json::from_value(excluded)
            .map_err(|e| StorageError::malformed("segment", e))?,
        rules: serde_json::from_value(rules).map_err(|e| StorageError::malformed("segment", e))?,
        version,
        created_at,
        updated_at,
    })
}
