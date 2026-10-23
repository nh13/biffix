//! `events` — the engine plugins' event delivery (`POST /events`, authorized by a run's capability
//! token rather than a user's login).

use serde::{Deserialize, Serialize};

/// A batch of engine events. Batched because the plugin buffers, and one POST per event would put a
/// network round trip in the engine's hot path.
///
/// Each element is an engine wire event, kept raw here and parsed one by one, so a kind this backend
/// does not know yet (a newer plugin's) is stored unfolded instead of failing the whole batch: an
/// event's kind is an open enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IngestRequest {
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
    pub events: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IngestSummary {
    /// Events durably appended by this call (excludes duplicates already in the log).
    pub appended: usize,
    /// Events accepted but already present, deduped on `dedup_key`. A plugin retry is a no-op.
    pub duplicates: usize,
    /// Events of the batch the route refused one by one and did not append (a `task_submitted`
    /// whose Batch job claim it could not verify), each with why. The rest of the batch landed; a
    /// resend of a refused event is refused again, so a poster logs it and drops it.
    #[serde(default)]
    pub refused: Vec<RefusedEvent>,
}

/// One event of an ingest batch the route refused, by its position in the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RefusedEvent {
    /// The event's index in the request's `events`.
    pub index: usize,
    /// Why it was refused.
    pub reason: String,
}
