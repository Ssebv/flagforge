//! Row types shared between the storage and API layers.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Member,
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    /// Whether this role may change configuration. Viewers get a read-only
    /// view of production, which is the point of having them.
    pub fn can_write(self) -> bool {
        matches!(self, Self::Owner | Self::Admin | Self::Member)
    }

    /// Whether this role may manage users and API keys.
    pub fn can_administer(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyScope {
    /// Full evaluation access; must stay on a server.
    Server,
    /// Intended for browsers and mobile apps.
    Client,
}

impl KeyScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "server" => Some(Self::Server),
            "client" => Some(Self::Client),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct User {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub email: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}

/// A user together with the secret needed to verify a login. Kept separate
/// from [`User`] so the hash cannot be serialized into a response by accident.
#[derive(Debug, Clone)]
pub struct UserWithSecret {
    pub user: User,
    pub password_hash: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Project {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Environment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub name: String,
    pub is_production: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Flag {
    pub id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub variants: Vec<flagforge_core::Variant>,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A flag's configuration inside one environment.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlagConfig {
    pub flag_id: Uuid,
    pub environment_id: Uuid,
    pub enabled: bool,
    pub off_variant: String,
    pub fallthrough: flagforge_core::Distribution,
    pub rules: Vec<flagforge_core::Rule>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

/// A reusable audience, as stored.
///
/// Carries the administrative fields the evaluation model has no use for —
/// name, timestamps, the row id — alongside the membership definition. The
/// engine only ever sees [`Segment::definition`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Segment {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub included: BTreeSet<String>,
    pub excluded: BTreeSet<String>,
    pub rules: Vec<flagforge_core::SegmentRule>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Segment {
    /// The evaluation view of this row.
    pub fn definition(&self) -> flagforge_core::Segment {
        flagforge_core::Segment {
            key: self.key.clone(),
            description: self.description.clone(),
            included: self.included.clone(),
            excluded: self.excluded.clone(),
            rules: self.rules.clone(),
            version: self.version,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentState {
    /// Defined but not yet measuring; the only state where the measurement
    /// fields may still be edited.
    Draft,
    /// Counting exposures and conversions; travels in the snapshot.
    Running,
    /// Finished. Terminal: reopening a measurement window would average two
    /// populations into an answer about neither.
    Stopped,
}

impl ExperimentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// An A/B experiment, as stored.
///
/// `flag_key` and `variants` are denormalized from the flag at read time: every
/// consumer — validation, the dashboard, the results view — needs them, and
/// making each one re-fetch the flag would turn one join into four queries.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Experiment {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub flag_id: Uuid,
    pub flag_key: String,
    pub variants: Vec<flagforge_core::Variant>,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub metric_key: String,
    pub control_variant: String,
    pub state: ExperimentState,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Experiment {
    /// The SDK-facing view, valid only while the experiment is running.
    pub fn spec(&self) -> flagforge_core::ExperimentSpec {
        flagforge_core::ExperimentSpec {
            key: self.key.clone(),
            flag_key: self.flag_key.clone(),
            metric_key: self.metric_key.clone(),
            control_variant: self.control_variant.clone(),
            version: self.version,
        }
    }
}

/// What one kind of counter counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CounterKind {
    /// The flag was evaluated for a context while the experiment ran.
    Exposure,
    /// A tracked metric event attributed to a variant.
    Conversion,
}

impl CounterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exposure => "exposure",
            Self::Conversion => "conversion",
        }
    }
}

/// One increment on its way into `experiment_counters`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterDelta {
    pub experiment_key: String,
    pub variant: String,
    pub kind: CounterKind,
    pub at: DateTime<Utc>,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiKey {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    /// Identifying prefix only — the secret itself is shown once, at creation.
    pub prefix: String,
    pub scope: KeyScope,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// What a presented SDK key resolves to.
#[derive(Debug, Clone)]
pub struct KeyIdentity {
    pub api_key_id: Uuid,
    pub environment_id: Uuid,
    pub project_id: Uuid,
    pub organization_id: Uuid,
    pub scope: KeyScope,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditEntry {
    pub id: i64,
    pub actor_email: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub environment_id: Option<Uuid>,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// An audit record about to be written.
#[derive(Debug, Clone)]
pub struct NewAuditEntry {
    pub organization_id: Uuid,
    pub actor_user_id: Option<Uuid>,
    pub actor_email: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub environment_id: Option<Uuid>,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}

impl NewAuditEntry {
    pub fn new(
        organization_id: Uuid,
        actor: (Option<Uuid>, &str),
        action: impl Into<String>,
        resource_type: impl Into<String>,
        resource_id: impl std::fmt::Display,
    ) -> Self {
        Self {
            organization_id,
            actor_user_id: actor.0,
            actor_email: actor.1.to_owned(),
            action: action.into(),
            resource_type: resource_type.into(),
            resource_id: resource_id.to_string(),
            environment_id: None,
            before: None,
            after: None,
        }
    }

    pub fn in_environment(mut self, environment_id: Uuid) -> Self {
        self.environment_id = Some(environment_id);
        self
    }

    pub fn changing<T: Serialize>(mut self, before: Option<&T>, after: Option<&T>) -> Self {
        self.before = before.and_then(|v| serde_json::to_value(v).ok());
        self.after = after.and_then(|v| serde_json::to_value(v).ok());
        self
    }
}
