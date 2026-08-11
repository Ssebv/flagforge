//! The SDK-facing event ingest endpoint.
//!
//! SDKs aggregate exposures and conversions locally and flush them here as
//! counter increments — counts, not events, so a busy process sends the same
//! few dozen bytes a quiet one does. The server adds them into hourly cells
//! in one round trip.

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Utc};
use flagforge_storage::experiments;
use flagforge_storage::models::{CounterDelta, CounterKind};
use serde::{Deserialize, Serialize};

use crate::auth::SdkIdentity;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Refuses absurd payloads before they become work; an SDK flushing every few
/// seconds accumulates nowhere near this many distinct cells.
const MAX_EVENTS: usize = 1_000;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct EventsRequest {
    pub events: Vec<EventDelta>,
}

/// One counter increment.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct EventDelta {
    /// Which experiment, by the key the snapshot announced.
    pub experiment_key: String,
    /// The variant the context was assigned; the SDK knows it because it
    /// evaluates locally with the same deterministic bucketing the server has.
    pub variant: String,
    pub kind: CounterKind,
    /// How many identical events this delta stands for.
    #[serde(default = "one")]
    pub count: u32,
    /// When the events happened; defaults to arrival time. SDKs that batch
    /// send the flush interval's timestamps so a counter lands in the hour it
    /// belongs to, not the hour the flush ran in.
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EventsResponse {
    /// Hourly counter cells written after aggregation. Zero against a
    /// non-empty batch means every event named an experiment that is unknown,
    /// stopped, or not yet started — routine when a batch straddles a stop,
    /// so it is reported rather than erroring.
    pub accepted: u64,
    pub received: usize,
}

/// Ingests a batch of counter increments for the key's environment.
#[utoipa::path(
    post, path = "/api/v1/events", tag = "evaluate",
    security(("sdk_key" = [])), request_body = EventsRequest,
    responses(
        (status = 202, description = "Counters updated", body = EventsResponse),
        (status = 400, description = "Too many events, or a malformed variant"),
        (status = 401, description = "Missing, unknown or revoked SDK key"),
        (status = 429, description = "Rate limited"),
    )
)]
pub async fn ingest(
    State(state): State<AppState>,
    identity: SdkIdentity,
    Json(body): Json<EventsRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<EventsResponse>)> {
    if body.events.len() > MAX_EVENTS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_EVENTS} events per batch; aggregate client-side and flush more often"
        )));
    }

    let received = body.events.len();
    let now = Utc::now();

    let mut deltas = Vec::with_capacity(received);
    for event in body.events {
        // Variants come from attacker-controllable input and land in a TEXT
        // column; the same charset rule keys live by keeps the table clean.
        if event.variant.is_empty() || event.variant.len() > 128 {
            return Err(ApiError::BadRequest(
                "variant must be between 1 and 128 characters".into(),
            ));
        }
        deltas.push(CounterDelta {
            experiment_key: event.experiment_key,
            variant: event.variant,
            kind: event.kind,
            at: event.at.unwrap_or(now),
            count: event.count,
        });
    }

    let accepted =
        experiments::ingest_counters(&state.pool, identity.environment_id(), &deltas).await?;

    metrics::counter!("flagforge_experiment_events_total").increment(accepted);

    Ok((axum::http::StatusCode::ACCEPTED, Json(EventsResponse { accepted, received })))
}
