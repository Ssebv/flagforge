//! A/B experiments and their pre-aggregated counters.

use std::collections::HashMap;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use flagforge_core::VariantCounts;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{FoundExt, Result, StorageError};
use crate::models::{CounterDelta, CounterKind, Experiment, ExperimentState};

/// The columns every experiment read selects: the row itself plus the flag's
/// key and variants, which every consumer needs (see [`Experiment`]).
macro_rules! experiment_query {
    ($condition:literal, $($arg:expr),* $(,)?) => {
        sqlx::query!(
            r#"
            SELECT e.id, e.environment_id, e.flag_id, f.key AS flag_key, f.variants,
                   e.key, e.name, e.description, e.metric_key, e.control_variant,
                   e.state, e.started_at, e.stopped_at, e.version, e.created_at, e.updated_at
            FROM experiments e
            JOIN flags f ON f.id = e.flag_id
            WHERE "# + $condition,
            $($arg),*
        )
    };
}

#[allow(clippy::too_many_arguments)]
pub async fn create_experiment(
    pool: &PgPool,
    environment_id: Uuid,
    flag_id: Uuid,
    key: &str,
    name: &str,
    description: Option<&str>,
    metric_key: &str,
    control_variant: &str,
) -> Result<Experiment> {
    sqlx::query!(
        r#"
        INSERT INTO experiments
            (environment_id, flag_id, key, name, description, metric_key, control_variant)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
        environment_id,
        flag_id,
        key,
        name,
        description,
        metric_key,
        control_variant,
    )
    .execute(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "experiment", key))?;

    find_experiment(pool, environment_id, key).await
}

pub async fn list_experiments(pool: &PgPool, environment_id: Uuid) -> Result<Vec<Experiment>> {
    let rows = experiment_query!("e.environment_id = $1 ORDER BY e.key", environment_id)
        .fetch_all(pool)
        .await?;

    rows.into_iter()
        .map(|r| {
            build(
                r.id,
                r.environment_id,
                r.flag_id,
                r.flag_key,
                r.variants,
                r.key,
                r.name,
                r.description,
                r.metric_key,
                r.control_variant,
                r.state,
                r.started_at,
                r.stopped_at,
                r.version,
                r.created_at,
                r.updated_at,
            )
        })
        .collect()
}

pub async fn find_experiment(pool: &PgPool, environment_id: Uuid, key: &str) -> Result<Experiment> {
    let r = experiment_query!("e.environment_id = $1 AND e.key = $2", environment_id, key)
        .fetch_optional(pool)
        .await?
        .or_not_found("experiment")?;

    build(
        r.id,
        r.environment_id,
        r.flag_id,
        r.flag_key,
        r.variants,
        r.key,
        r.name,
        r.description,
        r.metric_key,
        r.control_variant,
        r.state,
        r.started_at,
        r.stopped_at,
        r.version,
        r.created_at,
        r.updated_at,
    )
}

/// Rewrites an experiment's presentation fields.
///
/// The measurement fields — flag, metric, control — are deliberately not here:
/// changing what a running experiment measures would splice two different
/// questions into one answer. The API refuses those edits outside `draft`; a
/// draft's measurement fields are edited by deleting and recreating it, which
/// costs nothing because a draft has no counters yet.
pub async fn update_experiment(
    pool: &PgPool,
    environment_id: Uuid,
    key: &str,
    name: Option<&str>,
    description: Option<Option<&str>>,
) -> Result<Experiment> {
    // "leave alone" vs "set to NULL", the same split update_segment uses.
    let (clear_description, description_value) = match description {
        None => (false, None),
        Some(value) => (true, value),
    };

    let updated = sqlx::query!(
        r#"
        UPDATE experiments
        SET name        = COALESCE($3, name),
            description = CASE WHEN $4 THEN $5 ELSE description END
        WHERE environment_id = $1 AND key = $2
        "#,
        environment_id,
        key,
        name,
        clear_description,
        description_value,
    )
    .execute(pool)
    .await
    .map_err(|e| StorageError::from_write(e, "experiment", key))?;

    if updated.rows_affected() == 0 {
        return Err(StorageError::not_found("experiment"));
    }
    find_experiment(pool, environment_id, key).await
}

/// Moves a draft to `running`. One-way, enforced in the WHERE clause so two
/// racing starts cannot both succeed.
pub async fn start_experiment(
    pool: &PgPool,
    environment_id: Uuid,
    key: &str,
) -> Result<Experiment> {
    transition(pool, environment_id, key, ExperimentState::Draft, ExperimentState::Running).await
}

/// Moves a running experiment to `stopped`, its terminal state.
pub async fn stop_experiment(pool: &PgPool, environment_id: Uuid, key: &str) -> Result<Experiment> {
    transition(pool, environment_id, key, ExperimentState::Running, ExperimentState::Stopped).await
}

async fn transition(
    pool: &PgPool,
    environment_id: Uuid,
    key: &str,
    from: ExperimentState,
    to: ExperimentState,
) -> Result<Experiment> {
    let updated = sqlx::query!(
        r#"
        UPDATE experiments
        SET state      = $4,
            started_at = CASE WHEN $4 = 'running' THEN now() ELSE started_at END,
            stopped_at = CASE WHEN $4 = 'stopped' THEN now() ELSE stopped_at END
        WHERE environment_id = $1 AND key = $2 AND state = $3
        "#,
        environment_id,
        key,
        from.as_str(),
        to.as_str(),
    )
    .execute(pool)
    .await?;

    if updated.rows_affected() == 0 {
        // Nothing moved: either the experiment is missing or it is in the
        // wrong state. Reading it tells us which, and its actual state.
        let current = find_experiment(pool, environment_id, key).await?;
        return Err(StorageError::WrongState {
            entity: "experiment",
            key: key.to_owned(),
            actual: current.state.as_str(),
            needed: from.as_str(),
        });
    }
    find_experiment(pool, environment_id, key).await
}

/// Deletes an experiment and, by cascade, its counters.
///
/// A running experiment is refused: stopping is the explicit acknowledgement
/// that measurement is over, and deletion skipping it is usually a mis-click
/// about to destroy live results.
pub async fn delete_experiment(pool: &PgPool, environment_id: Uuid, key: &str) -> Result<()> {
    let deleted = sqlx::query!(
        r#"DELETE FROM experiments WHERE environment_id = $1 AND key = $2 AND state <> 'running'"#,
        environment_id,
        key,
    )
    .execute(pool)
    .await?;

    if deleted.rows_affected() == 0 {
        let current = find_experiment(pool, environment_id, key).await?;
        return Err(StorageError::WrongState {
            entity: "experiment",
            key: key.to_owned(),
            actual: current.state.as_str(),
            needed: "stopped (or still a draft)",
        });
    }
    Ok(())
}

/// Experiment keys that reference `flag_id`, qualified by environment so the
/// error message reading them locates each one. Used to refuse flag deletion.
pub async fn experiments_referencing_flag(pool: &PgPool, flag_id: Uuid) -> Result<Vec<String>> {
    let rows = sqlx::query!(
        r#"
        SELECT env.key AS environment_key, e.key
        FROM experiments e
        JOIN environments env ON env.id = e.environment_id
        WHERE e.flag_id = $1
        ORDER BY env.key, e.key
        "#,
        flag_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| format!("{}/{}", r.environment_key, r.key)).collect())
}

/// Applies a batch of counter increments in one round trip.
///
/// Returns how many increments were accepted. Deltas naming an experiment that
/// is unknown or not running are dropped rather than erroring: SDKs flush on a
/// timer, so a batch straddling an experiment's stop is routine traffic, not a
/// client bug to punish.
pub async fn ingest_counters(
    pool: &PgPool,
    environment_id: Uuid,
    deltas: &[CounterDelta],
) -> Result<u64> {
    // Collapse duplicates first: two deltas landing in the same counter cell
    // would make `ON CONFLICT DO UPDATE` touch a row twice in one statement,
    // which Postgres refuses outright.
    type Cell = (String, String, CounterKind, DateTime<Utc>);
    let mut cells: HashMap<Cell, i64> = HashMap::new();
    for delta in deltas {
        let hour = delta
            .at
            .duration_trunc(TimeDelta::hours(1))
            .map_err(|_| StorageError::Invalid { entity: "counter timestamp" })?;
        let cell = (delta.experiment_key.clone(), delta.variant.clone(), delta.kind, hour);
        *cells.entry(cell).or_default() += i64::from(delta.count);
    }

    let mut keys = Vec::with_capacity(cells.len());
    let mut variants = Vec::with_capacity(cells.len());
    let mut kinds = Vec::with_capacity(cells.len());
    let mut hours = Vec::with_capacity(cells.len());
    let mut counts = Vec::with_capacity(cells.len());
    for ((key, variant, kind, hour), count) in cells {
        keys.push(key);
        variants.push(variant);
        kinds.push(kind.as_str().to_owned());
        hours.push(hour);
        counts.push(count);
    }

    let written = sqlx::query!(
        r#"
        INSERT INTO experiment_counters (experiment_id, variant, kind, hour, count)
        SELECT e.id, d.variant, d.kind, d.hour, d.count
        FROM UNNEST($2::TEXT[], $3::TEXT[], $4::TEXT[], $5::TIMESTAMPTZ[], $6::BIGINT[])
             AS d(experiment_key, variant, kind, hour, count)
        JOIN experiments e
          ON e.environment_id = $1 AND e.key = d.experiment_key AND e.state = 'running'
        ON CONFLICT (experiment_id, variant, kind, hour)
        DO UPDATE SET count = experiment_counters.count + EXCLUDED.count
        "#,
        environment_id,
        &keys,
        &variants,
        &kinds,
        &hours,
        &counts,
    )
    .execute(pool)
    .await?;

    Ok(written.rows_affected())
}

/// Total exposures and conversions per variant, summed across every hour.
///
/// Variants appear only once they have a counter; the API layer fills in the
/// flag's other variants with zeroes so a results view shows every arm.
pub async fn experiment_counts(pool: &PgPool, experiment_id: Uuid) -> Result<Vec<VariantCounts>> {
    let rows = sqlx::query!(
        r#"
        SELECT variant, kind, SUM(count)::BIGINT AS "total!"
        FROM experiment_counters
        WHERE experiment_id = $1
        GROUP BY variant, kind
        ORDER BY variant
        "#,
        experiment_id,
    )
    .fetch_all(pool)
    .await?;

    let mut by_variant: Vec<VariantCounts> = Vec::new();
    for row in rows {
        let entry = match by_variant.iter_mut().find(|c| c.variant == row.variant) {
            Some(entry) => entry,
            None => {
                by_variant.push(VariantCounts {
                    variant: row.variant.clone(),
                    exposures: 0,
                    conversions: 0,
                });
                by_variant.last_mut().expect("just pushed")
            }
        };
        let total = u64::try_from(row.total).unwrap_or(0);
        match row.kind.as_str() {
            "exposure" => entry.exposures = total,
            _ => entry.conversions = total,
        }
    }
    Ok(by_variant)
}

/// The running experiments of one environment, for snapshot loading.
pub(crate) async fn load_running_experiments(
    pool: &PgPool,
    environment_id: Uuid,
) -> Result<Vec<flagforge_core::ExperimentSpec>> {
    let rows = sqlx::query!(
        r#"
        SELECT e.key, f.key AS flag_key, e.metric_key, e.control_variant, e.version
        FROM experiments e
        JOIN flags f ON f.id = e.flag_id
        WHERE e.environment_id = $1 AND e.state = 'running'
        ORDER BY e.key
        "#,
        environment_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| flagforge_core::ExperimentSpec {
            key: r.key,
            flag_key: r.flag_key,
            metric_key: r.metric_key,
            control_variant: r.control_variant,
            version: r.version,
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn build(
    id: Uuid,
    environment_id: Uuid,
    flag_id: Uuid,
    flag_key: String,
    variants: serde_json::Value,
    key: String,
    name: String,
    description: Option<String>,
    metric_key: String,
    control_variant: String,
    state: String,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> Result<Experiment> {
    Ok(Experiment {
        id,
        environment_id,
        flag_id,
        flag_key,
        variants: serde_json::from_value(variants)
            .map_err(|e| StorageError::malformed("experiment flag variants", e))?,
        key,
        name,
        description,
        metric_key,
        control_variant,
        state: ExperimentState::parse(&state)
            .ok_or(StorageError::Invalid { entity: "experiment state" })?,
        started_at,
        stopped_at,
        version,
        created_at,
        updated_at,
    })
}
