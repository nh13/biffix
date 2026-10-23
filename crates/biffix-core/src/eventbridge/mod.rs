//! Decode the ingest queue's messages: raw EventBridge events and biffix's own envelopes. Pure;
//! the ingest Lambda (and, for CodeBuild state changes, the backend sweep) does the IO.
//!
//! The queue carries exactly two message shapes, and [`route`] dispatches them by an explicit
//! discriminator rather than by probing which payload fields happen to be present:
//! - a **biffix-produced** message — a pre-normalized [`RunEvent`] wrapped in
//!   [`NormalizedIngestEnvelope`], stamped with the biffix-owned `biffix_ingest` tag (today the
//!   scheduled reconciler is the only such producer);
//! - a **raw EventBridge event** — whose shape AWS owns, classified by the standard
//!   `source` + `detail-type` envelope fields, then handed to [`decode`].
//!
//! Most messages decode to a [`RunEvent`]; ECS container-instance state changes decode to a
//! [`HostFact`] for the host registry instead (see [`Ingested`]). An unrecognized event is
//! dropped (the ingest drain logs it at debug level); nothing stores it.
//!
//! [`describe`] maps the same Batch terminal facts as `DescribeJobs` reports them, for the ingest
//! reconciler's backstop, onto the event [`decode`] makes of the live one.

pub mod decode;
pub mod describe;

use serde::{Deserialize, Serialize};

use crate::event::RunEvent;

use self::decode::{
    decode_batch, decode_codebuild, decode_ecs_host, decode_spot, DecodeError, HostFact,
    QueueContext,
};

/// The kind of biffix-produced ingest message. A single-variant enum so the wire value is a fixed,
/// validated string rather than free text; extend it if biffix grows another pre-normalized
/// producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BiffixIngestKind {
    /// A [`RunEvent`] already normalized by a biffix producer (today the scheduled reconciler).
    RunEvent,
}

/// The one typed envelope for messages biffix enqueues on the ingest queue itself.
///
/// It replaces duck-typing on incidental payload fields (`schema_version`/`dedup_key`): the producer
/// stamps the biffix-owned `biffix_ingest` tag and [`route`] dispatches on it, so adding fields to
/// [`RunEvent`] can never change routing and a raw AWS event (which never carries the tag) can never
/// be misread as normalized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedIngestEnvelope {
    /// The discriminator; present only on biffix-produced messages.
    pub biffix_ingest: BiffixIngestKind,
    /// The already-normalized event to append.
    pub event: RunEvent,
}

impl NormalizedIngestEnvelope {
    /// Wrap an already-normalized event for enqueue onto the ingest queue.
    pub fn new(event: RunEvent) -> Self {
        Self { biffix_ingest: BiffixIngestKind::RunEvent, event }
    }
}

/// What one ingest-queue message decodes to.
///
/// Both variants are large and each value lives only while one queue record is routed, so the size
/// difference between them costs nothing worth an indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Ingested {
    /// A normalized run event, appended to the event log and folded.
    Event(RunEvent),
    /// Host facts for the host registry; never enters the event log (see [`HostFact`]).
    Host(HostFact),
}

/// Decode one ingest-queue message, or `Ok(None)` if it's not one biffix ingests.
pub fn route(
    event: &serde_json::Value,
    ctx: &QueueContext,
) -> Result<Option<Ingested>, DecodeError> {
    // A message biffix produced itself is wrapped in the typed envelope and carries the
    // biffix-owned `biffix_ingest` tag. Its presence — and only its presence — marks the normalized
    // shape, so the two shapes on the queue are told apart deterministically (never by probing
    // which RunEvent fields happen to exist).
    if event.get("biffix_ingest").is_some() {
        return NormalizedIngestEnvelope::deserialize(event)
            .map(|envelope| Some(Ingested::Event(envelope.event)))
            .map_err(|error| {
                DecodeError::Malformed(format!("invalid biffix ingest envelope: {error}"))
            });
    }

    // Otherwise it is a raw EventBridge event (AWS owns this shape): classify by its standard fields.
    match eventbridge_type(event) {
        ("aws.batch", "Batch Job State Change") => {
            decode_batch(event, ctx).map(|e| Some(Ingested::Event(e)))
        }
        ("aws.ec2", "EC2 Spot Instance Interruption Warning") => {
            decode_spot(event).map(|e| Some(Ingested::Event(e)))
        }
        ("aws.ecs", "ECS Container Instance State Change") => {
            decode_ecs_host(event).map(|h| Some(Ingested::Host(h)))
        }
        CODEBUILD_STATE_CHANGE => decode_codebuild(event).map(|e| e.map(Ingested::Event)),
        _ => Ok(None),
    }
}

/// The EventBridge `(source, detail-type)` of a CodeBuild build state change: what [`route`]
/// decodes with [`decode_codebuild`].
pub(crate) const CODEBUILD_STATE_CHANGE: (&str, &str) =
    ("aws.codebuild", "CodeBuild Build State Change");

/// A raw EventBridge event's `(source, detail-type)`, empty where absent.
fn eventbridge_type(event: &serde_json::Value) -> (&str, &str) {
    let source = event.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let detail_type = event.get("detail-type").and_then(|v| v.as_str()).unwrap_or("");
    (source, detail_type)
}

/// A CodeBuild build state change decoded exactly as [`route`] decodes it (`Ok(None)`: a build
/// biffix did not start); `None` when `event` is no such change. For a consumer outside the ingest
/// queue (the backend sweep, which EventBridge also sends build state changes to), so the
/// classification lives here once.
pub fn codebuild_state_change(
    event: &serde_json::Value,
) -> Option<Result<Option<RunEvent>, DecodeError>> {
    (eventbridge_type(event) == CODEBUILD_STATE_CHANGE).then(|| decode_codebuild(event))
}
