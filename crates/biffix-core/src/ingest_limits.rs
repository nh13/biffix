//! The limits of the ingest route (`POST /events`) and the delivery policy the engine plugins'
//! posters follow against it.
//!
//! These constants are the one source: the backend enforces [`MAX_BATCH_EVENTS`] and
//! [`MAX_BODY_BYTES`], `cargo xtask emit-schemas` writes everything here to
//! `schemas/ingest-limits.json`, and each producer's test suite fails if its poster's constants
//! differ from that file (the Snakemake logger's `tests/test_ingest_limits.py`, nf-biffix's
//! `IngestPosterSpec`, and the coordinator's events.jsonl replay in
//! `docker/coordinator/tests/test_lib.py`).

use crate::event::Kind;

/// The most events one `POST /events` may carry; a larger batch is refused with 413. Enough for a
/// buffered plugin flush, small enough to bound one request's work.
pub const MAX_BATCH_EVENTS: usize = 500;

/// The request body cap of `POST /events`; a larger body is refused with 413. Set explicitly
/// rather than left at axum's 2 MiB default, which a full batch of legitimate events can exceed: a
/// `task_created` for a gather task lists every upstream task key (a 1,000-way scatter makes it
/// ~55 KB), and a `task_outputs` lists every file a task wrote. Sized to [`MAX_BATCH_EVENTS`]
/// events of ~10 KB, and under the 6 MB synchronous Lambda payload limit the deployed route sits
/// behind, so a body this route refuses would not have reached it anyway.
pub const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// The delivery policy both engine plugins' posters follow. Nothing in Rust sends events this way;
/// the values live here so the two plugins share one copy.
pub mod poster {
    use super::Kind;

    /// A batch's serialized size cap, below [`super::MAX_BODY_BYTES`]: room for the envelope, and
    /// for the 6 MB Lambda payload limit should the body be base64-encoded on the way
    /// (4 MiB * 4/3 < 6 MB).
    pub const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
    /// Events held while ingest is slow or unreachable; beyond this, events are dropped (the
    /// events file the coordinator replays at exit still carries them).
    pub const MAX_BUFFERED_EVENTS: usize = 20_000;
    /// Attempts per batch for a transient failure (connection error, 408, 429, 5xx).
    pub const MAX_ATTEMPTS: u32 = 3;
    /// Per-request timeout, in milliseconds.
    pub const REQUEST_TIMEOUT_MS: u64 = 5_000;
    /// The longest a batch waits for more events before it is sent, in milliseconds.
    pub const FLUSH_INTERVAL_MS: u64 = 1_000;
    /// The first retry's backoff, doubling per attempt, in milliseconds.
    pub const RETRY_BACKOFF_MS: u64 = 500;
    /// The longest a response's `Retry-After` is honoured for, in milliseconds.
    pub const MAX_RETRY_AFTER_MS: u64 = 30_000;
    /// The overall time budget of the poster's close (the flusher join plus the final drain), in
    /// milliseconds.
    pub const CLOSE_TIMEOUT_MS: u64 = 30_000;
    /// At most one "buffer full" warning per this interval, in milliseconds.
    pub const DROP_WARN_INTERVAL_MS: u64 = 30_000;
    /// POSTs one batch may spend isolating the events the route rejects (413/400) by halving it.
    pub const SPLIT_POST_BUDGET: u32 = 64;
    /// The most output refs one `task_outputs` event carries (about 120 KB of URIs); a task's
    /// outputs beyond it go in further events, which the fold unions. Without the cut, a task with
    /// a huge glob output would make one event over [`super::MAX_BODY_BYTES`], dropped whole.
    pub const MAX_OUTPUTS_PER_EVENT: usize = 1_000;
    /// The kinds kept when the buffer overflows (the others' oldest go first): `task_submitted`
    /// carries the binding that joins the AWS Batch stream to the engine's, and the run and task
    /// state transitions are small. An `engine_task_verdict` is one: a `failed` verdict is what
    /// fails an attempt AWS reports SUCCEEDED, so without it the task reads succeeded until the
    /// exit replay.
    pub const KEEP_UNDER_PRESSURE: &[Kind] = &[
        Kind::TaskSubmitted,
        Kind::TaskSucceeded,
        Kind::TaskFailed,
        Kind::TaskRetrying,
        Kind::TaskCached,
        Kind::RunStarted,
        Kind::RunSucceeded,
        Kind::RunFailed,
        Kind::EngineTaskVerdict,
    ];
}

/// The limits and policy as the JSON the plugins' tests read (`schemas/ingest-limits.json`).
pub fn to_json() -> serde_json::Value {
    let keep: Vec<&str> = poster::KEEP_UNDER_PRESSURE.iter().map(Kind::as_str).collect();
    serde_json::json!({
        "route": {
            "max_batch_events": MAX_BATCH_EVENTS,
            "max_body_bytes": MAX_BODY_BYTES,
        },
        "poster": {
            "max_batch_bytes": poster::MAX_BATCH_BYTES,
            "max_buffered_events": poster::MAX_BUFFERED_EVENTS,
            "max_attempts": poster::MAX_ATTEMPTS,
            "request_timeout_ms": poster::REQUEST_TIMEOUT_MS,
            "flush_interval_ms": poster::FLUSH_INTERVAL_MS,
            "retry_backoff_ms": poster::RETRY_BACKOFF_MS,
            "max_retry_after_ms": poster::MAX_RETRY_AFTER_MS,
            "close_timeout_ms": poster::CLOSE_TIMEOUT_MS,
            "drop_warn_interval_ms": poster::DROP_WARN_INTERVAL_MS,
            "split_post_budget": poster::SPLIT_POST_BUDGET,
            "max_outputs_per_event": poster::MAX_OUTPUTS_PER_EVENT,
            "keep_under_pressure": keep,
        },
    })
}

/// The synchronous Lambda payload limit the deployed route sits behind (6 MB, decimal).
const LAMBDA_PAYLOAD_BYTES: usize = 6_000_000;

// A poster batch fits the route, and fits the Lambda payload even base64-encoded (which grows it by
// 4/3); the route's own cap fits the payload only as sent, so a body near it could not arrive
// base64-encoded (no poster sends one). The buffer holds at least a full batch.
const _: () = assert!(poster::MAX_BATCH_BYTES < MAX_BODY_BYTES);
const _: () = assert!(MAX_BODY_BYTES < LAMBDA_PAYLOAD_BYTES);
const _: () = assert!(poster::MAX_BATCH_BYTES * 4 / 3 < LAMBDA_PAYLOAD_BYTES);
const _: () = assert!(poster::MAX_BUFFERED_EVENTS >= MAX_BATCH_EVENTS);
const _: () = assert!(poster::MAX_ATTEMPTS >= 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_names_every_value() {
        let json = to_json();
        assert_eq!(json["route"]["max_batch_events"], MAX_BATCH_EVENTS);
        assert_eq!(json["route"]["max_body_bytes"], MAX_BODY_BYTES);
        assert_eq!(json["poster"]["max_batch_bytes"], poster::MAX_BATCH_BYTES);
        assert_eq!(json["poster"]["keep_under_pressure"][0], Kind::TaskSubmitted.as_str());
        assert_eq!(json["poster"].as_object().unwrap().len(), 12);
    }

    #[test]
    fn every_kind_that_decides_a_task_s_state_is_kept() {
        for kind in [
            Kind::TaskSucceeded,
            Kind::TaskFailed,
            Kind::TaskRetrying,
            Kind::TaskCached,
            Kind::EngineTaskVerdict,
        ] {
            assert!(poster::KEEP_UNDER_PRESSURE.contains(&kind), "{kind:?}");
        }
    }

    #[test]
    fn the_kept_kinds_are_known_and_distinct() {
        let keep = poster::KEEP_UNDER_PRESSURE;
        for (i, kind) in keep.iter().enumerate() {
            assert!(kind.is_known(), "{kind:?}");
            assert!(!keep[..i].contains(kind), "{kind:?} listed twice");
        }
    }
}
