//! Typed client for the FlagForge API.
//!
//! Request and response shapes reuse `flagforge-core` wherever the server
//! does, so a change to `Rule` or `Distribution` breaks this crate at compile
//! time rather than at runtime in someone's browser.

use flagforge_core::{Distribution, Rule, SegmentRule, ValidationIssue, Variant};
use gloo_net::http::Request;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeSet;

/// Same origin as the app. Serving the SPA from the API binary means there is
/// no base URL to configure and no CORS preflight on the management API.
const BASE: &str = "/api/v1";

// ------------------------------------------------------------------ error --

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    /// The server's stable `type` slug, or `network` when the request never
    /// arrived.
    pub kind: String,
    pub title: String,
    /// Field-level problems, present on `validation_failed`.
    pub issues: Vec<ValidationIssue>,
}

impl ApiError {
    fn network(detail: impl std::fmt::Display) -> Self {
        Self {
            status: 0,
            kind: "network".into(),
            title: format!("could not reach the server: {detail}"),
            issues: Vec::new(),
        }
    }

    /// Whether the session is gone and the user has to sign in again.
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401
    }

    /// A lost optimistic-concurrency race, which the UI recovers from by
    /// reloading rather than by showing a dead end.
    pub fn is_conflict(&self) -> bool {
        self.status == 409
    }
}

// ------------------------------------------------------------ async state --

/// Explicit lifecycle for anything fetched over the network.
///
/// Modelling `Loading` and `Failed` as states rather than as an absent value
/// is what makes "spinner", "empty" and "something broke" three visibly
/// different screens instead of one blank panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(ApiError),
}

impl<T> From<Result<T, ApiError>> for Load<T> {
    fn from(result: Result<T, ApiError>) -> Self {
        match result {
            Ok(value) => Self::Ready(value),
            Err(error) => Self::Failed(error),
        }
    }
}

// ----------------------------------------------------------------- models --

pub mod models {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct User {
        pub id: String,
        pub organization_id: String,
        pub email: String,
        pub role: String,
        pub created_at: String,
    }

    impl User {
        pub fn can_write(&self) -> bool {
            matches!(self.role.as_str(), "owner" | "admin" | "member")
        }

        pub fn can_administer(&self) -> bool {
            matches!(self.role.as_str(), "owner" | "admin")
        }

        /// Initials for the sidebar avatar.
        pub fn initials(&self) -> String {
            self.email.chars().take(2).collect::<String>().to_uppercase()
        }
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Organization {
        pub id: String,
        pub name: String,
        pub slug: String,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Session {
        pub token: String,
        pub expires_in: u64,
        pub user: User,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Me {
        pub user: User,
        pub organization: Organization,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Project {
        pub id: String,
        pub key: String,
        pub name: String,
        pub description: Option<String>,
        pub created_at: String,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Environment {
        pub id: String,
        pub key: String,
        pub name: String,
        pub is_production: bool,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Flag {
        pub id: String,
        pub key: String,
        pub name: String,
        pub description: Option<String>,
        pub variants: Vec<Variant>,
        pub archived: bool,
        pub updated_at: String,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct FlagConfig {
        pub flag_id: String,
        pub environment_id: String,
        pub enabled: bool,
        pub off_variant: String,
        pub fallthrough: Distribution,
        pub rules: Vec<Rule>,
        pub version: i64,
        pub updated_at: String,
    }

    /// A flag together with how it is configured in one environment — what the
    /// dashboard renders as a single row.
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct ConfiguredFlag {
        pub flag: Flag,
        pub config: FlagConfig,
    }

    /// A reusable audience, as the management API returns it.
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Segment {
        pub id: String,
        pub key: String,
        pub name: String,
        pub description: Option<String>,
        #[serde(default)]
        pub included: BTreeSet<String>,
        #[serde(default)]
        pub excluded: BTreeSet<String>,
        #[serde(default)]
        pub rules: Vec<SegmentRule>,
        pub version: i64,
        pub updated_at: String,
    }

    impl Segment {
        /// Roughly how many ways in there are, for the list view. Not a member
        /// count — that would need the whole population — but it does
        /// distinguish "defined" from "empty".
        pub fn is_empty(&self) -> bool {
            self.included.is_empty() && self.rules.is_empty()
        }
    }

    /// A segment plus the flags whose rules name it.
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct SegmentWithUsage {
        #[serde(flatten)]
        pub segment: Segment,
        #[serde(default)]
        pub referenced_by: Vec<String>,
    }

    /// An A/B experiment, as the management API returns it.
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Experiment {
        pub id: String,
        pub key: String,
        pub name: String,
        pub description: Option<String>,
        pub flag_key: String,
        /// The flag's variants, denormalized so the results view can render
        /// every arm without a second request.
        pub variants: Vec<Variant>,
        pub metric_key: String,
        pub control_variant: String,
        /// `draft`, `running` or `stopped`.
        pub state: String,
        pub started_at: Option<String>,
        pub stopped_at: Option<String>,
        pub version: i64,
    }

    /// An experiment next to its judged results.
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct ExperimentResults {
        pub experiment: Experiment,
        pub results: Vec<flagforge_core::VariantResult>,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct ApiKey {
        pub id: String,
        pub name: String,
        pub prefix: String,
        pub scope: String,
        pub created_at: String,
        pub last_used_at: Option<String>,
        pub revoked_at: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct CreatedKey {
        #[serde(flatten)]
        pub key: ApiKey,
        pub secret: String,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct AuditEntry {
        pub id: i64,
        pub actor_email: String,
        pub action: String,
        pub resource_type: String,
        pub resource_id: String,
        pub before: Option<Value>,
        pub after: Option<Value>,
        pub created_at: String,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct AuditPage {
        pub entries: Vec<AuditEntry>,
        pub next_cursor: Option<i64>,
    }
}

use models::*;

// --------------------------------------------------------------- requests --

#[derive(Serialize)]
pub struct RegisterBody<'a> {
    pub organization_name: &'a str,
    pub email: &'a str,
    pub password: &'a str,
}

#[derive(Serialize)]
pub struct LoginBody<'a> {
    pub email: &'a str,
    pub password: &'a str,
}

#[derive(Serialize)]
pub struct NewProject<'a> {
    pub key: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
}

#[derive(Serialize)]
pub struct NewEnvironment<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub is_production: bool,
}

#[derive(Serialize)]
pub struct NewFlag<'a> {
    pub key: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<Variant>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallthrough: Option<Distribution>,
}

#[derive(Serialize)]
pub struct ConfigBody {
    pub enabled: bool,
    pub off_variant: String,
    pub fallthrough: Distribution,
    pub rules: Vec<Rule>,
    /// Always sent. Every write from the dashboard is guarded, so two people
    /// editing the same flag get a clear conflict instead of a silent
    /// overwrite.
    pub expected_version: i64,
}

#[derive(Serialize)]
pub struct NewSegment<'a> {
    pub key: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
}

/// A full rewrite of a segment's membership. Every field is sent, so the
/// dashboard never has to reason about which half of a patch it omitted.
#[derive(Serialize)]
pub struct SegmentBody {
    pub name: String,
    pub included: BTreeSet<String>,
    pub excluded: BTreeSet<String>,
    pub rules: Vec<SegmentRule>,
    /// Always sent, for the same reason `ConfigBody` sends it: one segment
    /// edit moves every flag that references it, so a silent overwrite here is
    /// wider than a silent overwrite of one flag.
    pub expected_version: i64,
}

#[derive(Serialize)]
pub struct NewExperiment<'a> {
    pub key: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub flag_key: &'a str,
    pub metric_key: &'a str,
    pub control_variant: &'a str,
}

#[derive(Serialize)]
pub struct NewKey<'a> {
    pub name: &'a str,
    pub scope: &'a str,
}

#[derive(Serialize)]
pub struct ArchiveBody {
    pub archived: bool,
}

// ----------------------------------------------------------------- client --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

async fn send<T: DeserializeOwned>(
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<T, ApiError> {
    let url = format!("{BASE}{path}");

    let builder = match method {
        Method::Get => Request::get(&url),
        Method::Post => Request::post(&url),
        Method::Put => Request::put(&url),
        Method::Patch => Request::patch(&url),
        Method::Delete => Request::delete(&url),
    };

    let builder = match token {
        Some(token) => builder.header("authorization", &format!("Bearer {token}")),
        None => builder,
    };

    let request = match body {
        Some(json) => builder.json(&json).map_err(ApiError::network)?,
        None => builder.build().map_err(ApiError::network)?,
    };

    let response = request.send().await.map_err(ApiError::network)?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();

    if !(200..300).contains(&status) {
        // Error bodies are RFC 9457 problem documents. A body that is not one
        // means something in front of the API answered — a proxy, a gateway —
        // so fall back to the status rather than reporting a parse failure.
        let problem: Option<Value> = serde_json::from_str(&text).ok();
        return Err(ApiError {
            status,
            kind: problem
                .as_ref()
                .and_then(|p| p["type"].as_str())
                .unwrap_or("unexpected")
                .to_owned(),
            title: problem
                .as_ref()
                .and_then(|p| p["title"].as_str())
                .unwrap_or("the request failed")
                .to_owned(),
            issues: problem
                .as_ref()
                .and_then(|p| serde_json::from_value(p["errors"].clone()).ok())
                .unwrap_or_default(),
        });
    }

    // 204s and other empty successes deserialize as `null`, so callers can use
    // `()` as the response type.
    let text = if text.trim().is_empty() { "null".to_owned() } else { text };
    serde_json::from_str(&text)
        .map_err(|e| ApiError::network(format!("unexpected response shape: {e}")))
}

fn json(value: impl Serialize) -> Option<Value> {
    serde_json::to_value(value).ok()
}

// ------------------------------------------------------------------ calls --

pub async fn register(body: RegisterBody<'_>) -> Result<Session, ApiError> {
    send(Method::Post, "/auth/register", None, json(body)).await
}

pub async fn login(body: LoginBody<'_>) -> Result<Session, ApiError> {
    send(Method::Post, "/auth/login", None, json(body)).await
}

pub async fn me(token: &str) -> Result<Me, ApiError> {
    send(Method::Get, "/auth/me", Some(token), None).await
}

pub async fn list_projects(token: &str) -> Result<Vec<Project>, ApiError> {
    send(Method::Get, "/projects", Some(token), None).await
}

pub async fn create_project(token: &str, body: NewProject<'_>) -> Result<Project, ApiError> {
    send(Method::Post, "/projects", Some(token), json(body)).await
}

pub async fn delete_project(token: &str, key: &str) -> Result<(), ApiError> {
    send(Method::Delete, &format!("/projects/{key}"), Some(token), None).await
}

pub async fn list_environments(token: &str, project: &str) -> Result<Vec<Environment>, ApiError> {
    send(Method::Get, &format!("/projects/{project}/environments"), Some(token), None).await
}

pub async fn create_environment(
    token: &str,
    project: &str,
    body: NewEnvironment<'_>,
) -> Result<Environment, ApiError> {
    send(Method::Post, &format!("/projects/{project}/environments"), Some(token), json(body)).await
}

pub async fn create_flag(token: &str, project: &str, body: NewFlag<'_>) -> Result<Flag, ApiError> {
    send(Method::Post, &format!("/projects/{project}/flags"), Some(token), json(body)).await
}

pub async fn get_flag(token: &str, project: &str, flag: &str) -> Result<Flag, ApiError> {
    send(Method::Get, &format!("/projects/{project}/flags/{flag}"), Some(token), None).await
}

pub async fn archive_flag(
    token: &str,
    project: &str,
    flag: &str,
    archived: bool,
) -> Result<Flag, ApiError> {
    send(
        Method::Patch,
        &format!("/projects/{project}/flags/{flag}"),
        Some(token),
        json(ArchiveBody { archived }),
    )
    .await
}

pub async fn delete_flag(token: &str, project: &str, flag: &str) -> Result<(), ApiError> {
    send(Method::Delete, &format!("/projects/{project}/flags/{flag}"), Some(token), None).await
}

/// Every flag in the project with its configuration here, in one round trip.
pub async fn list_configured(
    token: &str,
    project: &str,
    environment: &str,
) -> Result<Vec<ConfiguredFlag>, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/flags"),
        Some(token),
        None,
    )
    .await
}

pub async fn get_config(
    token: &str,
    project: &str,
    environment: &str,
    flag: &str,
) -> Result<FlagConfig, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/flags/{flag}"),
        Some(token),
        None,
    )
    .await
}

pub async fn put_config(
    token: &str,
    project: &str,
    environment: &str,
    flag: &str,
    body: ConfigBody,
) -> Result<FlagConfig, ApiError> {
    send(
        Method::Put,
        &format!("/projects/{project}/environments/{environment}/flags/{flag}"),
        Some(token),
        json(body),
    )
    .await
}

pub async fn list_segments(
    token: &str,
    project: &str,
    environment: &str,
) -> Result<Vec<Segment>, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/segments"),
        Some(token),
        None,
    )
    .await
}

pub async fn get_segment(
    token: &str,
    project: &str,
    environment: &str,
    segment: &str,
) -> Result<SegmentWithUsage, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/segments/{segment}"),
        Some(token),
        None,
    )
    .await
}

pub async fn create_segment(
    token: &str,
    project: &str,
    environment: &str,
    body: NewSegment<'_>,
) -> Result<Segment, ApiError> {
    send(
        Method::Post,
        &format!("/projects/{project}/environments/{environment}/segments"),
        Some(token),
        json(body),
    )
    .await
}

pub async fn put_segment(
    token: &str,
    project: &str,
    environment: &str,
    segment: &str,
    body: SegmentBody,
) -> Result<Segment, ApiError> {
    send(
        Method::Put,
        &format!("/projects/{project}/environments/{environment}/segments/{segment}"),
        Some(token),
        json(body),
    )
    .await
}

pub async fn delete_segment(
    token: &str,
    project: &str,
    environment: &str,
    segment: &str,
) -> Result<(), ApiError> {
    send(
        Method::Delete,
        &format!("/projects/{project}/environments/{environment}/segments/{segment}"),
        Some(token),
        None,
    )
    .await
}

pub async fn list_experiments(
    token: &str,
    project: &str,
    environment: &str,
) -> Result<Vec<Experiment>, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/experiments"),
        Some(token),
        None,
    )
    .await
}

pub async fn create_experiment(
    token: &str,
    project: &str,
    environment: &str,
    body: NewExperiment<'_>,
) -> Result<Experiment, ApiError> {
    send(
        Method::Post,
        &format!("/projects/{project}/environments/{environment}/experiments"),
        Some(token),
        json(body),
    )
    .await
}

/// `action` is `start` or `stop` — the two lifecycle transitions.
pub async fn transition_experiment(
    token: &str,
    project: &str,
    environment: &str,
    experiment: &str,
    action: &str,
) -> Result<Experiment, ApiError> {
    send(
        Method::Post,
        &format!(
            "/projects/{project}/environments/{environment}/experiments/{experiment}/{action}"
        ),
        Some(token),
        Some(Value::Null),
    )
    .await
}

pub async fn experiment_results(
    token: &str,
    project: &str,
    environment: &str,
    experiment: &str,
) -> Result<ExperimentResults, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/experiments/{experiment}/results"),
        Some(token),
        None,
    )
    .await
}

pub async fn delete_experiment(
    token: &str,
    project: &str,
    environment: &str,
    experiment: &str,
) -> Result<(), ApiError> {
    send(
        Method::Delete,
        &format!("/projects/{project}/environments/{environment}/experiments/{experiment}"),
        Some(token),
        None,
    )
    .await
}

pub async fn list_keys(
    token: &str,
    project: &str,
    environment: &str,
) -> Result<Vec<ApiKey>, ApiError> {
    send(
        Method::Get,
        &format!("/projects/{project}/environments/{environment}/keys"),
        Some(token),
        None,
    )
    .await
}

pub async fn create_key(
    token: &str,
    project: &str,
    environment: &str,
    body: NewKey<'_>,
) -> Result<CreatedKey, ApiError> {
    send(
        Method::Post,
        &format!("/projects/{project}/environments/{environment}/keys"),
        Some(token),
        json(body),
    )
    .await
}

pub async fn revoke_key(
    token: &str,
    project: &str,
    environment: &str,
    id: &str,
) -> Result<(), ApiError> {
    send(
        Method::Delete,
        &format!("/projects/{project}/environments/{environment}/keys/{id}"),
        Some(token),
        None,
    )
    .await
}

pub async fn audit(token: &str, limit: u32, before: Option<i64>) -> Result<AuditPage, ApiError> {
    let mut path = format!("/audit?limit={limit}");
    if let Some(cursor) = before {
        path.push_str(&format!("&before_id={cursor}"));
    }
    send(Method::Get, &path, Some(token), None).await
}
