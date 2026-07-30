//! Organizations and users.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{FoundExt, Result, StorageError};
use crate::models::{Organization, Role, User, UserWithSecret};

/// Creates an organization and its first user in one transaction.
///
/// Signup is all-or-nothing: an organization with no owner would be
/// unreachable, and a user with no organization has nothing to administer.
pub async fn create_organization_with_owner(
    pool: &PgPool,
    org_name: &str,
    slug: &str,
    email: &str,
    password_hash: &str,
) -> Result<(Organization, User)> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    let org = sqlx::query!(
        r#"
        INSERT INTO organizations (name, slug)
        VALUES ($1, $2)
        RETURNING id, name, slug, created_at
        "#,
        org_name,
        slug,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| StorageError::from_write(e, "organization", slug))?;

    let email = email.trim().to_lowercase();
    let user = sqlx::query!(
        r#"
        INSERT INTO users (organization_id, email, password_hash, role)
        VALUES ($1, $2, $3, 'owner')
        RETURNING id, organization_id, email, role, created_at
        "#,
        org.id,
        email,
        password_hash,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| StorageError::from_write(e, "user", &email))?;

    tx.commit().await?;

    Ok((
        Organization { id: org.id, name: org.name, slug: org.slug, created_at: org.created_at },
        User {
            id: user.id,
            organization_id: user.organization_id,
            email: user.email,
            role: Role::parse(&user.role).unwrap_or(Role::Viewer),
            created_at: user.created_at,
        },
    ))
}

/// Looks a user up for login. Returns the password hash, so callers must not
/// leak the result into a response.
pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<UserWithSecret>> {
    let email = email.trim().to_lowercase();
    let row = sqlx::query!(
        r#"
        SELECT id, organization_id, email, password_hash, role, created_at
        FROM users
        WHERE lower(email) = $1
        "#,
        email,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| UserWithSecret {
        user: User {
            id: r.id,
            organization_id: r.organization_id,
            email: r.email,
            role: Role::parse(&r.role).unwrap_or(Role::Viewer),
            created_at: r.created_at,
        },
        password_hash: r.password_hash,
    }))
}

pub async fn find_user(pool: &PgPool, id: Uuid) -> Result<User> {
    sqlx::query!(
        r#"
        SELECT id, organization_id, email, role, created_at
        FROM users
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?
    .map(|r| User {
        id: r.id,
        organization_id: r.organization_id,
        email: r.email,
        role: Role::parse(&r.role).unwrap_or(Role::Viewer),
        created_at: r.created_at,
    })
    .or_not_found("user")
}

pub async fn find_organization(pool: &PgPool, id: Uuid) -> Result<Organization> {
    sqlx::query_as!(
        Organization,
        r#"SELECT id, name, slug, created_at FROM organizations WHERE id = $1"#,
        id,
    )
    .fetch_optional(pool)
    .await?
    .or_not_found("organization")
}
