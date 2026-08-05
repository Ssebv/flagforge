//! Demo data.
//!
//! A freshly deployed FlagForge is an empty dashboard, which is the worst
//! possible first impression of a product whose whole point is what it shows
//! you. `flagforge seed` fills it with a plausible organization so the first
//! screen anyone sees has flags on it — half-rolled-out, targeted, one already
//! archived, the way a real project looks a month in.
//!
//! Deliberately not run automatically at start-up: creating accounts in
//! someone's database because they booted a binary would be a surprise.

use std::collections::BTreeSet;

use flagforge_core::{
    AttributeValue, Condition, Distribution, Operator, Rule, SegmentMatch, SegmentRollout,
    SegmentRule, TOTAL_WEIGHT, Variant, WeightedVariant,
};
use flagforge_storage::models::{KeyScope, NewAuditEntry};
use flagforge_storage::{PgPool, accounts, api_keys, audit, flags, projects, segments};
use uuid::Uuid;

use crate::auth::{keys, password};
use crate::routes::new_salt;

/// The one demo segment, referenced by a flag rule below.
const BETA_SEGMENT: &str = "beta-testers";

pub struct Credentials {
    pub email: String,
    pub password: String,
}

impl Default for Credentials {
    fn default() -> Self {
        Self {
            email: "ada@acme.test".to_owned(),
            password: "correct-horse-battery-staple".to_owned(),
        }
    }
}

/// Populates an empty database and prints what was created.
pub async fn run(pool: &PgPool, credentials: Credentials) -> anyhow::Result<()> {
    if accounts::find_by_email(pool, &credentials.email).await?.is_some() {
        anyhow::bail!(
            "`{}` already exists — seeding twice would create a confusing duplicate. \
             Drop the database or pass a different --email.",
            credentials.email
        );
    }

    let hash = password::hash(&credentials.password)
        .map_err(|e| anyhow::anyhow!("could not hash the demo password: {e}"))?;

    let (organization, user) = accounts::create_organization_with_owner(
        pool,
        "Acme Inc",
        "acme-inc",
        &credentials.email,
        &hash,
    )
    .await?;

    let actor = (Some(user.id), user.email.as_str());
    let project = projects::create_project(
        pool,
        organization.id,
        "checkout",
        "Checkout",
        Some("Cart, payment and order confirmation."),
    )
    .await?;

    // Environments first: a flag is seeded into every environment that exists
    // when it is created, so the order matters.
    let mut environments = Vec::new();
    for (key, name, production) in
        [("production", "Production", true), ("staging", "Staging", false)]
    {
        environments.push(
            projects::create_environment(pool, project.id, key, name, &new_salt(), production)
                .await?,
        );
    }

    // Segments before flags: a rule may only name a segment its environment
    // already defines, so seeding the other way round would be refused by the
    // same check that protects a real write.
    for environment in &environments {
        let segment = segments::create_segment(
            pool,
            environment.id,
            BETA_SEGMENT,
            "Beta testers",
            Some("Opted-in accounts, plus a slice of enterprise traffic."),
        )
        .await?;

        // Narrower in production than in staging — which is the reason
        // segments are environment-scoped in the first place.
        let rules = if environment.is_production {
            vec![SegmentRule {
                rollout: Some(SegmentRollout { percentage: 20_000, bucket_by: None }),
                ..SegmentRule::new(
                    Uuid::from_u128(90),
                    vec![Condition::new(
                        "plan",
                        Operator::In,
                        vec![AttributeValue::String("enterprise".into())],
                    )],
                )
            }]
        } else {
            vec![SegmentRule::new(Uuid::from_u128(91), Vec::new())]
        };

        segments::update_segment(
            pool,
            environment.id,
            &segment.key,
            None,
            None,
            Some(&BTreeSet::from(["user-7".to_owned(), "user-42".to_owned()])),
            Some(&BTreeSet::from(["user-13".to_owned()])),
            Some(&rules),
            None,
        )
        .await?;

        audit::record(
            pool,
            NewAuditEntry::new(
                organization.id,
                actor,
                "segment.created",
                "segment",
                format!("checkout/{}/{}", environment.key, BETA_SEGMENT),
            )
            .in_environment(environment.id)
            .changing(None, Some(&segment)),
        )
        .await?;
    }

    for definition in catalogue() {
        let flag = flags::create_flag(
            pool,
            project.id,
            definition.key,
            definition.name,
            Some(definition.description),
            &definition.variants,
            definition.off_variant,
            &Distribution::fixed(definition.off_variant),
        )
        .await?;

        for environment in &environments {
            let config = if environment.is_production {
                &definition.production
            } else {
                &definition.staging
            };

            flags::upsert_config(
                pool,
                flag.id,
                environment.id,
                config.enabled,
                definition.off_variant,
                &config.fallthrough,
                &config.rules,
                None,
            )
            .await?;
        }

        if definition.archived {
            flags::update_flag(pool, flag.id, None, None, None, Some(true)).await?;
        }

        audit::record(
            pool,
            NewAuditEntry::new(
                organization.id,
                actor,
                "flag.created",
                "flag",
                format!("checkout/{}", definition.key),
            )
            .changing(None, Some(&flag)),
        )
        .await?;
    }

    // One key per environment, printed once — exactly as the dashboard does.
    let mut secrets = Vec::new();
    for environment in &environments {
        let generated = keys::generate(KeyScope::Server);
        api_keys::create(
            pool,
            environment.id,
            &format!("{}-backend", environment.key),
            &generated.hash,
            &generated.prefix,
            KeyScope::Server,
        )
        .await?;
        secrets.push((environment.key.clone(), generated.secret));
    }

    report(&credentials, &secrets);
    Ok(())
}

fn report(credentials: &Credentials, secrets: &[(String, String)]) {
    println!("\nSeeded a demo organization.\n");
    println!("  Sign in at /");
    println!("    email     {}", credentials.email);
    println!("    password  {}", credentials.password);
    println!("\n  SDK keys (shown once — only their hashes are stored):");
    for (environment, secret) in secrets {
        println!("    {environment:<12} {secret}");
    }
    println!(
        "\n  Try it:\n    \
         curl -s -X POST http://localhost:8080/api/v1/evaluate/checkout.v2 \\\n      \
         -H \"authorization: Bearer {}\" -H 'content-type: application/json' \\\n      \
         -d '{{\"context\":{{\"key\":\"user-42\",\"attributes\":{{\"plan\":\"pro\"}}}}}}'\n",
        secrets.first().map(|(_, s)| s.as_str()).unwrap_or("<key>")
    );
}

struct Seeded {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    variants: Vec<Variant>,
    off_variant: &'static str,
    production: EnvConfig,
    staging: EnvConfig,
    archived: bool,
}

struct EnvConfig {
    enabled: bool,
    fallthrough: Distribution,
    rules: Vec<Rule>,
}

fn boolean_variants() -> Vec<Variant> {
    vec![Variant::new("on", true), Variant::new("off", false)]
}

fn rule(
    id: u128,
    attribute: &str,
    operator: Operator,
    values: Vec<AttributeValue>,
    serve: &str,
) -> Rule {
    Rule::new(
        Uuid::from_u128(id),
        vec![Condition::new(attribute, operator, values)],
        Distribution::fixed(serve),
    )
}

fn rollout(on: u32) -> Distribution {
    Distribution::Rollout {
        weights: vec![
            WeightedVariant { variant: "on".into(), weight: on },
            WeightedVariant { variant: "off".into(), weight: TOTAL_WEIGHT - on },
        ],
        bucket_by: None,
    }
}

/// A spread of the shapes a real project accumulates: a canary, a targeted
/// beta, a kill switch, a multivariate experiment and something left over.
fn catalogue() -> Vec<Seeded> {
    vec![
        Seeded {
            key: "checkout.v2",
            name: "New checkout",
            description: "Rebuilt checkout flow. Rolling out gradually.",
            variants: boolean_variants(),
            off_variant: "off",
            production: EnvConfig {
                enabled: true,
                fallthrough: rollout(25_000),
                rules: vec![{
                    let mut r = rule(
                        1,
                        "plan",
                        Operator::In,
                        vec!["enterprise".into(), "pro".into()],
                        "on",
                    );
                    r.description = Some("Paid plans get it immediately".into());
                    r
                }],
            },
            staging: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("on"),
                rules: Vec::new(),
            },
            archived: false,
        },
        Seeded {
            key: "delivery.express",
            name: "Express delivery",
            description: "Same-day option at checkout. Spain only for now.",
            variants: boolean_variants(),
            off_variant: "off",
            production: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("off"),
                rules: vec![{
                    let mut r = rule(2, "country", Operator::In, vec!["ES".into()], "on");
                    r.description = Some("Only where we have the couriers".into());
                    r
                }],
            },
            staging: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("on"),
                rules: Vec::new(),
            },
            archived: false,
        },
        Seeded {
            key: "payments.new-provider",
            name: "New payment provider",
            description: "Kill switch for the provider migration.",
            variants: boolean_variants(),
            off_variant: "off",
            production: EnvConfig {
                enabled: true,
                fallthrough: rollout(1_000),
                rules: vec![{
                    let mut r =
                        Rule::new(Uuid::from_u128(3), Vec::new(), Distribution::fixed("on"))
                            .targeting(SegmentMatch::any_of([BETA_SEGMENT]));
                    r.description = Some("Beta testers get the new provider first".into());
                    r
                }],
            },
            staging: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("on"),
                rules: Vec::new(),
            },
            archived: false,
        },
        Seeded {
            key: "loyalty.banner",
            name: "Loyalty banner",
            description: "Which banner variant to show above the cart.",
            variants: vec![
                Variant::new("control", "control"),
                Variant::new("points", "points"),
                Variant::new("discount", "discount"),
                Variant::new("off", false),
            ],
            off_variant: "off",
            production: EnvConfig {
                enabled: true,
                fallthrough: Distribution::Rollout {
                    weights: vec![
                        WeightedVariant { variant: "control".into(), weight: 34_000 },
                        WeightedVariant { variant: "points".into(), weight: 33_000 },
                        WeightedVariant { variant: "discount".into(), weight: 33_000 },
                    ],
                    bucket_by: None,
                },
                rules: Vec::new(),
            },
            staging: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("points"),
                rules: Vec::new(),
            },
            archived: false,
        },
        Seeded {
            key: "search.rerank",
            name: "Search reranking",
            description: "Internal only while we measure relevance.",
            variants: boolean_variants(),
            off_variant: "off",
            production: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("off"),
                rules: vec![{
                    let mut r =
                        rule(3, "email", Operator::EndsWith, vec!["@acme.test".into()], "on");
                    r.description = Some("Employees only".into());
                    r
                }],
            },
            staging: EnvConfig {
                enabled: true,
                fallthrough: Distribution::fixed("on"),
                rules: Vec::new(),
            },
            archived: false,
        },
        Seeded {
            key: "cart.legacy-banner",
            name: "Legacy cart banner",
            description: "Shipped and cleaned up. Kept for the audit trail.",
            variants: boolean_variants(),
            off_variant: "off",
            production: EnvConfig {
                enabled: false,
                fallthrough: Distribution::fixed("off"),
                rules: Vec::new(),
            },
            staging: EnvConfig {
                enabled: false,
                fallthrough: Distribution::fixed("off"),
                rules: Vec::new(),
            },
            archived: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the seed writes goes through the same validator the API
    /// uses, so a demo that would not survive a real request is caught here.
    #[test]
    fn every_seeded_configuration_is_evaluable() {
        for definition in catalogue() {
            for (label, config) in
                [("production", &definition.production), ("staging", &definition.staging)]
            {
                let candidate = flagforge_core::Flag {
                    key: definition.key.to_owned(),
                    variants: definition.variants.clone(),
                    enabled: config.enabled,
                    off_variant: definition.off_variant.to_owned(),
                    fallthrough: config.fallthrough.clone(),
                    rules: config.rules.clone(),
                    version: 1,
                };

                if let Err(issues) = flagforge_core::validate(&candidate) {
                    panic!("{} in {label} is invalid: {issues:?}", definition.key);
                }
            }
        }
    }

    #[test]
    fn the_catalogue_covers_the_shapes_worth_showing() {
        let catalogue = catalogue();

        assert!(catalogue.iter().any(|f| f.archived), "an archived flag");
        assert!(catalogue.iter().any(|f| f.variants.len() > 2), "a multivariate flag");
        assert!(
            catalogue
                .iter()
                .any(|f| matches!(f.production.fallthrough, Distribution::Rollout { .. })),
            "a percentage rollout"
        );
        assert!(catalogue.iter().any(|f| !f.production.rules.is_empty()), "a targeting rule");
        assert!(catalogue.iter().any(|f| !f.production.enabled), "a flag that is off");
    }
}
