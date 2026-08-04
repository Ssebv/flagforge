//! Projects and their environments.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{FoundExt, Result, StorageError};
use crate::models::{Environment, Project};

pub async fn create_project(
    pool: &PgPool,
    organization_id: Uuid,
    key: &str,
    name: &str,
    description: Option<&str>,
) -> Result<Project> {
    sqlx::query_as!(
        Project,
        r#"
        INSERT INTO projects (organization_id, key, name, description)
        VALUES ($1, $2, $3, $4)
        RETURNING id, organization_id, key, name, description, created_at
        "#,
        organization_id,
        key,
        name,
        description,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "project", key))
}

pub async fn list_projects(pool: &PgPool, organization_id: Uuid) -> Result<Vec<Project>> {
    let rows = sqlx::query_as!(
        Project,
        r#"
        SELECT id, organization_id, key, name, description, created_at
        FROM projects
        WHERE organization_id = $1
        ORDER BY key
        "#,
        organization_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetches a project scoped to its organization.
///
/// Every lookup takes the caller's `organization_id` rather than trusting the
/// path parameter: a tenant must not be able to reach another tenant's project
/// just by guessing a UUID.
pub async fn find_project(pool: &PgPool, organization_id: Uuid, key: &str) -> Result<Project> {
    sqlx::query_as!(
        Project,
        r#"
        SELECT id, organization_id, key, name, description, created_at
        FROM projects
        WHERE organization_id = $1 AND key = $2
        "#,
        organization_id,
        key,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("project")
}

pub async fn delete_project(pool: &PgPool, organization_id: Uuid, key: &str) -> Result<()> {
    let deleted = sqlx::query!(
        r#"DELETE FROM projects WHERE organization_id = $1 AND key = $2"#,
        organization_id,
        key,
    )
    .execute(pool)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(StorageError::not_found("project"));
    }
    Ok(())
}

/// Creates an environment and seeds every existing flag into it, disabled.
///
/// The mirror image of [`crate::flags::create_flag`], which seeds a new flag
/// into every environment that already exists. Both halves are needed for the
/// same invariant: *a flag is configured in every environment of its project*.
/// With only one half, adding `staging` to a project that already has flags
/// left them with no configuration there — invisible in the dashboard, and
/// absent from the snapshot, so an SDK fell back to its hard-coded default
/// with no way to tell that from "the flag is off".
pub async fn create_environment(
    pool: &PgPool,
    project_id: Uuid,
    key: &str,
    name: &str,
    salt: &str,
    is_production: bool,
) -> Result<Environment> {
    let mut tx = pool.begin().await?;

    let environment = sqlx::query_as!(
        Environment,
        r#"
        INSERT INTO environments (project_id, key, name, salt, is_production)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, project_id, key, name, is_production, created_at
        "#,
        project_id,
        key,
        name,
        salt,
        is_production,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| StorageError::from_write(e, "environment", key))?;

    // What each flag serves here comes from the oldest environment that
    // already configures it: the flag was seeded into all of them with the
    // same `off_variant` and `fallthrough` at creation, so the oldest is the
    // closest thing to "what this flag was defined as". Rules are deliberately
    // not copied — targeting is what differs between environments, and
    // inheriting production's rules into a fresh environment would be a
    // surprise rather than a convenience.
    //
    // The `defaults` branch covers a flag created while the project had no
    // environments at all, so there is no configuration anywhere to copy: it
    // falls back to the flag's own `off` variant, or to its first one.
    sqlx::query!(
        r#"
        INSERT INTO flag_configs (flag_id, environment_id, enabled, off_variant, fallthrough, rules)
        SELECT
            f.id,
            $1,
            FALSE,
            COALESCE(reference.off_variant, defaults.variant),
            COALESCE(
                reference.fallthrough,
                jsonb_build_object('kind', 'fixed', 'variant', defaults.variant)
            ),
            '[]'::JSONB
        FROM flags f
        LEFT JOIN LATERAL (
            SELECT c.off_variant, c.fallthrough
            FROM flag_configs c
            JOIN environments e ON e.id = c.environment_id
            WHERE c.flag_id = f.id
            ORDER BY e.created_at, e.id
            LIMIT 1
        ) reference ON TRUE
        LEFT JOIN LATERAL (
            SELECT element ->> 'key' AS variant
            FROM jsonb_array_elements(f.variants) WITH ORDINALITY AS variants(element, position)
            ORDER BY (element ->> 'key' <> 'off'), position
            LIMIT 1
        ) defaults ON TRUE
        WHERE f.project_id = $2
        "#,
        environment.id,
        project_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(environment)
}

pub async fn list_environments(pool: &PgPool, project_id: Uuid) -> Result<Vec<Environment>> {
    let rows = sqlx::query_as!(
        Environment,
        r#"
        SELECT id, project_id, key, name, is_production, created_at
        FROM environments
        WHERE project_id = $1
        ORDER BY is_production, key
        "#,
        project_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn find_environment(pool: &PgPool, project_id: Uuid, key: &str) -> Result<Environment> {
    sqlx::query_as!(
        Environment,
        r#"
        SELECT id, project_id, key, name, is_production, created_at
        FROM environments
        WHERE project_id = $1 AND key = $2
        "#,
        project_id,
        key,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("environment")
}

pub async fn delete_environment(pool: &PgPool, project_id: Uuid, key: &str) -> Result<()> {
    let deleted = sqlx::query!(
        r#"DELETE FROM environments WHERE project_id = $1 AND key = $2"#,
        project_id,
        key,
    )
    .execute(pool)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(StorageError::not_found("environment"));
    }
    Ok(())
}
