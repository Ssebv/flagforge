//! Storage errors, expressed in terms the API layer can map to status codes
//! without knowing anything about Postgres.

/// Postgres SQLSTATE for `unique_violation`.
const UNIQUE_VIOLATION: &str = "23505";
/// Postgres SQLSTATE for `foreign_key_violation`.
const FOREIGN_KEY_VIOLATION: &str = "23503";
/// Postgres SQLSTATE for `check_violation`.
const CHECK_VIOLATION: &str = "23514";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{entity} not found")]
    NotFound { entity: &'static str },

    #[error("{entity} `{key}` already exists")]
    Conflict { entity: &'static str, key: String },

    /// An optimistic-concurrency check failed: the caller's `expected_version`
    /// no longer matches, meaning someone else wrote first.
    #[error("{entity} was modified by someone else (you were working from version {expected})")]
    VersionConflict { entity: &'static str, expected: i64 },

    /// A write that violated a CHECK constraint — the database refusing input
    /// the application layer should have rejected first.
    #[error("{entity} failed a database constraint")]
    Invalid { entity: &'static str },

    /// A delete refused because something still points at the row. Postgres
    /// cannot enforce this one: the reference lives inside a JSONB rule, not in
    /// a column a foreign key could cover.
    #[error("{entity} `{key}` is still referenced by {}", referenced_by.join(", "))]
    InUse { entity: &'static str, key: String, referenced_by: Vec<String> },

    /// A row we wrote no longer deserializes into the domain model. This means
    /// a schema/model skew, not a user error, so it is never a 4xx.
    #[error("stored {entity} is malformed: {source}")]
    Malformed {
        entity: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, StorageError>;

impl StorageError {
    pub fn not_found(entity: &'static str) -> Self {
        Self::NotFound { entity }
    }

    /// Translates driver-level constraint violations into domain errors.
    ///
    /// Doing this here — rather than checking for existence first — keeps
    /// creates atomic: a concurrent insert of the same key yields a clean
    /// `Conflict` instead of a lost race between SELECT and INSERT.
    pub fn from_write(error: sqlx::Error, entity: &'static str, key: impl Into<String>) -> Self {
        match error.as_database_error().and_then(|e| e.code()).as_deref() {
            Some(UNIQUE_VIOLATION) => Self::Conflict { entity, key: key.into() },
            Some(FOREIGN_KEY_VIOLATION) => Self::NotFound { entity },
            Some(CHECK_VIOLATION) => Self::Invalid { entity },
            _ => Self::Database(error),
        }
    }

    pub fn malformed(entity: &'static str, source: serde_json::Error) -> Self {
        Self::Malformed { entity, source }
    }
}

/// `Option` -> `Result`, for the many queries whose "no rows" case is a 404.
pub trait FoundExt<T> {
    fn or_not_found(self, entity: &'static str) -> Result<T>;
}

impl<T> FoundExt<T> for Option<T> {
    fn or_not_found(self, entity: &'static str) -> Result<T> {
        self.ok_or(StorageError::NotFound { entity })
    }
}
