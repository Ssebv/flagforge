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

pub async fn create_environment(
    pool: &PgPool,
    project_id: Uuid,
    key: &str,
    name: &str,
    salt: &str,
    is_production: bool,
) -> Result<Environment> {
    sqlx::query_as!(
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
    .fetch_one(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "environment", key))
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
