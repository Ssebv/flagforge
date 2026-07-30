//! The OpenAPI document and its Swagger UI.
//!
//! Generated from the handlers themselves, so the documentation cannot drift
//! from the code the way a hand-written spec does.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::routes::{audit, auth, evaluate, flags, health, keys, projects};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "FlagForge",
        version = env!("CARGO_PKG_VERSION"),
        description = "A multi-tenant feature-flag service: targeted rollouts, \
                       deterministic bucketing and an audit trail.",
        license(name = "MIT"),
    ),
    tags(
        (name = "health", description = "Liveness, readiness and metrics"),
        (name = "auth", description = "Registration and access tokens"),
        (name = "projects", description = "Projects"),
        (name = "environments", description = "Environments within a project"),
        (name = "flags", description = "Flag definitions and per-environment configuration"),
        (name = "keys", description = "SDK keys"),
        (name = "audit", description = "Change history"),
        (name = "evaluate", description = "The SDK-facing evaluation API"),
    ),
    paths(
        health::live,
        health::ready,
        auth::register,
        auth::login,
        auth::me,
        projects::create,
        projects::list,
        projects::get,
        projects::delete,
        projects::create_environment,
        projects::list_environments,
        projects::delete_environment,
        flags::create,
        flags::list,
        flags::get,
        flags::update,
        flags::delete,
        flags::get_config,
        flags::update_config,
        keys::create,
        keys::list,
        keys::revoke,
        audit::list,
        evaluate::evaluate_all,
        evaluate::evaluate_one,
        evaluate::snapshot,
    ),
    components(schemas(
        crate::error::ProblemDetails,
        flagforge_core::Flag,
        flagforge_core::Variant,
        flagforge_core::VariantValue,
        flagforge_core::AttributeValue,
        flagforge_core::Distribution,
        flagforge_core::WeightedVariant,
        flagforge_core::Condition,
        flagforge_core::Operator,
        flagforge_core::Rule,
        flagforge_core::EvaluationContext,
        flagforge_core::Evaluation,
        flagforge_core::Reason,
        flagforge_core::ValidationIssue,
        flagforge_storage::models::Organization,
        flagforge_storage::models::User,
        flagforge_storage::models::Role,
        flagforge_storage::models::Project,
        flagforge_storage::models::Environment,
        flagforge_storage::models::Flag,
        flagforge_storage::models::FlagConfig,
        flagforge_storage::models::ApiKey,
        flagforge_storage::models::KeyScope,
        flagforge_storage::models::AuditEntry,
    )),
    modifiers(&SecuritySchemes),
)]
pub struct ApiDoc;

/// Declares the two credential types the API accepts.
struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);

        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some("Access token from /api/v1/auth/login"))
                    .build(),
            ),
        );

        components.add_security_scheme(
            "sdk_key",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "Authorization",
                "An SDK key as `Bearer ff_srv_...` or `ff_cli_...`",
            ))),
        );
    }
}

/// Swagger UI, mounted only outside production.
///
/// The document itself describes every management endpoint; serving an
/// interactive console for them on a public production host is an invitation
/// nobody needs to send.
pub fn swagger_ui() -> axum::Router {
    utoipa_swagger_ui::SwaggerUi::new("/docs")
        .url("/openapi.json", <ApiDoc as OpenApi>::openapi())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_document_builds_and_covers_every_route_group() {
        let document = <ApiDoc as OpenApi>::openapi();
        let json = serde_json::to_string(&document).unwrap();

        for path in [
            "/api/v1/auth/login",
            "/api/v1/projects",
            "/api/v1/evaluate",
            "/api/v1/audit",
            "/health",
        ] {
            assert!(json.contains(path), "missing {path} from the OpenAPI document");
        }
    }

    #[test]
    fn both_credential_types_are_declared() {
        let document = <ApiDoc as OpenApi>::openapi();
        let components = document.components.expect("components");

        assert!(components.security_schemes.contains_key("bearer"));
        assert!(components.security_schemes.contains_key("sdk_key"));
    }

    /// utoipa keys schemas by type name, so two types called `Flag` would
    /// leave one silently overwriting the other and every `$ref` to it wrong.
    #[test]
    fn the_domain_and_storage_flags_do_not_collide() {
        let document = <ApiDoc as OpenApi>::openapi();
        let json = serde_json::to_value(&document).unwrap();
        let schemas = &json["components"]["schemas"];

        let stored = schemas["Flag"]["properties"].as_object().expect("stored Flag schema");
        let defined =
            schemas["FlagDefinition"]["properties"].as_object().expect("domain Flag schema");

        // The stored record is the management view; the definition is what the
        // engine evaluates. Neither should be standing in for the other.
        assert!(stored.contains_key("archived") && stored.contains_key("project_id"));
        assert!(defined.contains_key("fallthrough") && defined.contains_key("rules"));
        assert!(!stored.contains_key("rules"), "the stored record absorbed the domain schema");
    }

    #[test]
    fn the_document_never_mentions_a_bucketing_salt() {
        // `EnvironmentSnapshot` is exposed to SDKs; the salt must not be part
        // of its documented shape any more than of its serialized form.
        let json = serde_json::to_string(&<ApiDoc as OpenApi>::openapi()).unwrap();
        assert!(!json.contains("\"salt\""), "the OpenAPI document leaks the salt field");
    }
}
