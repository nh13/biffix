//! Per-source translators: wire event → normalized [`RunEvent`].
//!
//! Deliberately dumb: one wire event → one RunEvent, no status/retry/cost decisions (those are
//! projections). The AWS translator here is the exact mapping the ingest Lambda uses.
//!
//! A wire value this build does not know (a new Batch/CodeBuild status, EC2 event kind or engine
//! kind) is not dropped: it becomes an [`EventData::Unknown`] built by [`EventData::unknown`] —
//! stored and audited under a non-empty kind, with the raw wire event nested, but not folded.

use serde::Deserialize;

use crate::dedup;
use crate::event::{EventData, Kind, RunEvent, TerminationCategory};
use crate::ids::RunId;
use crate::source::EventSource;
use crate::time::Timestamp;
use crate::wire::aws_batch::{AwsBatchEvent, BatchStatus};
use crate::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};
use crate::wire::aws_ec2::{Ec2Event, Ec2EventKind};
use crate::wire::engine::{EngineBody, EngineEvent};

/// AWS Batch Job State Change → RunEvent.
///
/// `run_id` is left `None`; the fold stitches it when `task_submitted` binds the `batch_job_id`.
/// Worker vs coordinator is told apart by `is_coordinator` (set from the job's queue upstream).
pub fn from_aws_batch(ev: &AwsBatchEvent) -> RunEvent {
    let dedup_key = dedup::aws_batch(&ev.batch_job_id, &ev.status, ev.prior_attempts.unwrap_or(0));

    let data = if ev.is_coordinator {
        match ev.status {
            BatchStatus::Submitted | BatchStatus::Pending | BatchStatus::Runnable => {
                EventData::CoordinatorSubmitted {}
            }
            BatchStatus::Starting | BatchStatus::Running => {
                EventData::CoordinatorRunning { log_stream: ev.log_stream.clone() }
            }
            BatchStatus::Succeeded => {
                EventData::CoordinatorSucceeded { log_stream: ev.log_stream.clone() }
            }
            // Batch's account of the stop, kept as the worker branch keeps it: for a coordinator
            // that never started it is the only record of why.
            BatchStatus::Failed => EventData::CoordinatorFailed {
                log_stream: ev.log_stream.clone(),
                exit_code: ev.exit_code,
                status_reason: ev.status_reason.clone(),
                termination_category: category_from_reason(ev.status_reason.as_deref()),
            },
            BatchStatus::Unknown(_) => unknown_aws_batch(ev),
        }
    } else {
        match ev.status {
            BatchStatus::Submitted | BatchStatus::Pending | BatchStatus::Runnable => {
                EventData::TaskQueued {}
            }
            BatchStatus::Starting | BatchStatus::Running => EventData::TaskStarted {
                started_at: Some(ev.ts),
                log_stream: ev.log_stream.clone(),
            },
            BatchStatus::Succeeded => EventData::TaskSucceeded {
                exit_code: ev.exit_code.unwrap_or(0),
                log_stream: ev.log_stream.clone(),
            },
            BatchStatus::Failed => EventData::TaskFailed {
                exit_code: ev.exit_code.unwrap_or(-1),
                termination_category: category_from_reason(ev.status_reason.as_deref()),
                status_reason: ev.status_reason.clone(),
                log_stream: ev.log_stream.clone(),
            },
            BatchStatus::Unknown(_) => unknown_aws_batch(ev),
        }
    };

    RunEvent {
        batch_job_id: Some(ev.batch_job_id.clone()),
        instance_id: ev.instance_id.clone(),
        queue: ev.queue.clone(),
        requested_vcpus: ev.requested_vcpus,
        requested_memory_mib: ev.requested_memory_mib,
        ..RunEvent::new(dedup_key, ev.ts, EventSource::AwsBatch, data)
    }
}

/// The stored payload of a Batch state change with a status this build does not know.
fn unknown_aws_batch(ev: &AwsBatchEvent) -> EventData {
    EventData::unknown(format!("aws_batch_{}", ev.status.as_wire().to_ascii_lowercase()), raw(ev))
}

/// A wire event as JSON, to nest under an unknown payload's `raw`.
fn raw<T: serde::Serialize>(ev: &T) -> serde_json::Value {
    serde_json::to_value(ev).unwrap_or(serde_json::Value::Null)
}

/// A Batch job's stop reason for the record: its `statusReason`, with the container's own `reason`
/// after it when that says more (a job that never started only says "Task failed to start"; the
/// container reason says why, e.g. `CannotPullContainerError: … not found`).
pub fn batch_reason(status_reason: Option<&str>, container_reason: Option<&str>) -> Option<String> {
    let status = status_reason.map(str::trim).filter(|s| !s.is_empty());
    let container = container_reason.map(str::trim).filter(|s| !s.is_empty());
    match (status, container) {
        (Some(s), Some(c)) if s != c && !s.contains(c) => Some(format!("{s}: {c}")),
        (Some(s), _) => Some(s.to_string()),
        (None, c) => c.map(str::to_string),
    }
}

/// Derive a coarse termination category from the Batch status reason. The precise
/// classification is a projection concern; this is just the translator's best hint.
///
/// The reason is free text that can quote image names and paths (`batch_reason` appends the
/// container's reason, e.g. `CannotPullContainerError: …/bloom:1`), so it matches whole words and
/// the specific markers Batch/ECS use, never substrings: `room`/`bloom` are not `oom`, `hotspot`
/// is not `spot`, and a message that merely mentions memory is not an out-of-memory kill. A
/// container that never started (a `Cannot…ContainerError`) was not reclaimed or OOM-killed, so it
/// gets no category.
pub(crate) fn category_from_reason(reason: Option<&str>) -> Option<TerminationCategory> {
    let r = reason?.to_ascii_lowercase();
    let words = reason_words(&r);
    let has = |word: &str| words.contains(&word);
    if never_started(&words) {
        None
    } else if has("spot") {
        Some(TerminationCategory::Spot)
    } else if has("host") && words.iter().any(|w| w.starts_with("terminat")) {
        Some(TerminationCategory::HostTerminated)
    } else if ["outofmemory", "outofmemoryerror", "oomkilled", "oom"]
        .iter()
        .any(|marker| has(marker))
        || r.contains("out of memory")
        || r.contains("killed due to memory usage")
    {
        Some(TerminationCategory::Oom)
    } else {
        None
    }
}

/// The words of an already lower-cased status reason, split on anything not alphanumeric: the
/// unit every reason rule matches, so `bloom` never reads as `oom`.
pub(crate) fn reason_words(lowercased_reason: &str) -> Vec<&str> {
    lowercased_reason
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether a reason's [`reason_words`] name a container that never started: any ECS
/// `Cannot…ContainerError` (pull, create, start, inspect).
pub(crate) fn never_started(words: &[&str]) -> bool {
    words.iter().any(|w| w.starts_with("cannot") && w.ends_with("containererror"))
}

/// EC2 spot interruption → RunEvent. Attribution instance→task is a projection join.
pub fn from_aws_ec2(ev: &Ec2Event) -> RunEvent {
    let (data, dedup_key) = match &ev.kind {
        Ec2EventKind::SpotInterruptionWarning => (
            EventData::SpotReclaimWarning {},
            dedup::aws_ec2_interruption(&ev.instance_id, ev.ts.as_micros()),
        ),
        // Lowercased like the Batch and CodeBuild siblings, and keyed on its kind so it never
        // meets a spot warning (or another kind) at the same instance and time.
        Ec2EventKind::Unknown(kind) => {
            let kind = kind.to_ascii_lowercase();
            (
                EventData::unknown(format!("aws_ec2_{kind}"), raw(ev)),
                dedup::aws_ec2(&ev.instance_id, &kind, ev.ts.as_micros()),
            )
        }
    };
    RunEvent {
        instance_id: Some(ev.instance_id.clone()),
        ..RunEvent::new(dedup_key, ev.ts, EventSource::AwsEc2, data)
    }
}

/// CodeBuild build state change → RunEvent. `run_id` stays `None`: a build is not part of a run;
/// its
/// `subject` is the fold key.
pub fn from_aws_codebuild(ev: &CodeBuildEvent) -> RunEvent {
    let subject = ev.subject.clone();
    let build_arn = ev.build_arn.clone();
    let logs_url = ev.logs_url.clone();
    let data = match &ev.status {
        CodeBuildStatus::InProgress => EventData::BuildStarted { subject, build_arn, logs_url },
        CodeBuildStatus::Succeeded => {
            EventData::BuildSucceeded { subject, build_arn, outputs: ev.outputs.clone(), logs_url }
        }
        CodeBuildStatus::Failed
        | CodeBuildStatus::Fault
        | CodeBuildStatus::Stopped
        | CodeBuildStatus::TimedOut => EventData::BuildFailed {
            subject,
            build_arn,
            reason: match &ev.reason {
                Some(reason) => format!("{}: {reason}", ev.status.as_str()),
                None => ev.status.as_str().to_string(),
            },
            logs_url,
        },
        CodeBuildStatus::Unknown(status) => {
            EventData::unknown(format!("aws_codebuild_{}", status.to_ascii_lowercase()), raw(ev))
        }
    };
    let dedup_key = dedup::aws_codebuild(&ev.build_arn, ev.status.as_str());
    RunEvent::new(dedup_key, ev.ts, EventSource::AwsCodebuild, data)
}

/// Engine wire event → RunEvent, stamping the caller-supplied engine `source`. The engine wire
/// shape is engine-agnostic (both the Snakemake logger and the Nextflow observer POST the same
/// [`EngineEvent`]), so the only per-engine difference is which `EventSource` the fact is recorded
/// under — the ingest handler resolves the run's engine and passes it here, keeping engine
/// provenance in the log correct rather than labelling every engine event `EventSource::Snakemake`.
pub fn from_engine(source: EventSource, ev: &EngineEvent) -> RunEvent {
    // Correlation fields + typed payload per engine body. The wire `kind` tag is NOT hand-typed
    // here — it is derived below from `data.kind().as_str()`, the single source of truth, so it
    // cannot drift from the payload variant.
    let (task_key, attempt, batch_job_id, data) = match &ev.body {
        EngineBody::RunStarted { session_id } => {
            (None, None, None, EventData::RunStarted { session_id: session_id.clone() })
        }
        EngineBody::RunProgress { completed, total } => {
            (None, None, None, EventData::RunProgress { completed: *completed, total: *total })
        }
        EngineBody::RunSucceeded => (None, None, None, EventData::RunSucceeded {}),
        EngineBody::RunFailed => {
            (None, None, None, EventData::RunFailed { reason: None, stage: None })
        }
        EngineBody::RunDag { dag } => (None, None, None, EventData::RunDag { dag: dag.clone() }),
        EngineBody::TaskCreated { task_key, group, deps } => (
            Some(task_key.clone()),
            None,
            None,
            EventData::TaskCreated { group: group.clone(), deps: deps.clone() },
        ),
        EngineBody::TaskSubmitted { task_key, attempt, batch_job_id, image } => (
            Some(task_key.clone()),
            Some(*attempt),
            Some(batch_job_id.clone()),
            EventData::TaskSubmitted { image: image.clone() },
        ),
        EngineBody::EngineTaskVerdict { task_key, attempt, verdict, category, reason } => (
            Some(task_key.clone()),
            *attempt,
            None,
            EventData::EngineTaskVerdict {
                verdict: verdict.clone(),
                category: category.clone(),
                reason: reason.clone(),
            },
        ),
        // A local rule's terminals carry the engine's attempt when it retries the rule; it rides in
        // the RunEvent's `attempt`, and so in the dedup key's attempt slot (empty when absent: no
        // attempt slot).
        EngineBody::TaskSucceeded { task_key, attempt, .. } => (
            Some(task_key.clone()),
            *attempt,
            None,
            // Engine-reported success for a local rule: no Batch exit code or log stream exists,
            // so record 0 and none.
            EventData::TaskSucceeded { exit_code: 0, log_stream: None },
        ),
        EngineBody::TaskCached { task_key, attempt, .. } => (
            Some(task_key.clone()),
            *attempt,
            None,
            // Cache-hit: no Batch job, no exit code — folds to a succeeded terminal flagged cached.
            EventData::TaskCached {},
        ),
        EngineBody::TaskFailed { task_key, attempt, .. } => (
            Some(task_key.clone()),
            *attempt,
            None,
            // Engine-reported failure for a local rule: no Batch exit/category/reason/log.
            EventData::TaskFailed {
                exit_code: -1,
                termination_category: None,
                status_reason: None,
                log_stream: None,
            },
        ),
        EngineBody::TaskRetrying { task_key, prev_attempt, next_attempt } => (
            Some(task_key.clone()),
            None,
            None,
            EventData::TaskRetrying { prev_attempt: *prev_attempt, next_attempt: *next_attempt },
        ),
        EngineBody::TaskOutputs { task_key, attempt, role, reused, outputs } => (
            task_key.clone(),
            *attempt,
            None,
            EventData::TaskOutputs {
                role: role.clone(),
                reused: *reused,
                outputs: outputs.clone(),
            },
        ),
    };
    // Per-fact discriminator for the *repeatable* engine kinds, so the log's uniqueness
    // constraint does not collapse distinct facts into one. `run_progress` recurs
    // per run, `task_outputs` per batch of outputs, `task_retrying` per retry, `task_submitted`
    // per real Batch submission, `run_dag` per distinct graph — each needs a distinct key while a
    // redelivery of the *same* fact still dedupes. Singleton kinds (`run_started`/`run_succeeded`/
    // …) keep their stable, discriminator-free key. The disc is the trailing key component, so a
    // value containing `:` (an s3 uri, or the `completed:total` pair below) is safe as long as no
    // task key ends in `:<attempt>:<kind>` (see the INVARIANT on `dedup::engine`).
    let disc: Option<String> = match &ev.body {
        // Both `completed` AND `total` identify a progress fact: a checkpoint can grow `total`
        // while `completed` is unchanged, and keying on `completed` alone dropped that update.
        EngineBody::RunProgress { completed, total } => Some(format!("{completed}:{total}")),
        // One batch of outputs is identified by its role and its set of URIs (order-independent),
        // so a redelivery dedupes while a second batch for the same attempt (a Nextflow publish
        // flush) is a distinct fact that unions in the fold. Hex, so the `:` invariant holds.
        EngineBody::TaskOutputs { role, reused, outputs, .. } => {
            Some(task_outputs_disc(role.as_ref(), *reused, outputs))
        }
        // A graph is identified by its node and edge sets (order-independent), so a redelivery of
        // the same graph dedupes while a re-emitted, grown graph (a checkpoint adding rules)
        // reaches the fold's set-union instead of being dropped as a redelivery.
        EngineBody::RunDag { dag } => Some(run_dag_disc(dag)),
        EngineBody::TaskRetrying { next_attempt, .. } => Some(next_attempt.0.to_string()),
        // Two submissions of the same (run, task, attempt) that land on *different* Batch jobs are
        // distinct facts — the Batch job id disambiguates them, so a re-submit is stored and can
        // bind rather than being dropped as a redelivery of the first.
        EngineBody::TaskSubmitted { batch_job_id, .. } => Some(batch_job_id.as_str().to_string()),
        // One verdict of each kind per (task, attempt): a redelivery dedupes, whatever its reason.
        // An attempt-less verdict covers what was submitted by its `ts`, so each time it is sent
        // is its own fact: the key carries the ts.
        EngineBody::EngineTaskVerdict { verdict, attempt, .. } => Some(match attempt {
            Some(_) => verdict.as_str().to_string(),
            None => format!("{}:{}", verdict.as_str(), ev.ts.as_micros()),
        }),
        // A local terminal that names its execution (Nextflow re-runs a task at the same attempt
        // on an executor retry) is that execution's own fact. Absent ⇒ no disc.
        EngineBody::TaskSucceeded { execution_id, .. }
        | EngineBody::TaskCached { execution_id, .. }
        | EngineBody::TaskFailed { execution_id, .. } => execution_id.clone(),
        _ => None,
    };
    // The wire kind tag comes from the payload itself (`Kind::as_str`), never a parallel literal.
    let kind = data.kind();
    let dedup_key = dedup::engine(
        source.as_str(),
        &ev.run_id,
        task_key.as_ref(),
        attempt.map(|a| a.0),
        kind.as_str(),
        disc.as_deref(),
    );
    RunEvent {
        run_id: Some(ev.run_id.clone()),
        task_key,
        attempt,
        batch_job_id,
        ..RunEvent::new(dedup_key, ev.ts, source, data)
    }
}

/// Why one element of an engine batch was rejected ([`from_engine_value`]).
#[derive(Debug, thiserror::Error)]
pub enum EngineEventError {
    #[error("event has no string `kind`")]
    MissingKind,
    /// A kind this build knows, with a body that does not parse: the producer is broken, not newer.
    #[error("malformed `{kind}` event: {error}")]
    Malformed { kind: String, error: serde_json::Error },
    /// A kind no engine may produce: a known non-engine RunEvent kind (e.g. `run_submitted`,
    /// `coordinator_failed`), or an unknown kind outside the engine namespace
    /// ([`ENGINE_KIND_PREFIX`]). Storing either, even unfolded, would put a control-plane or AWS
    /// fact's name on an engine's claim — and the log is immutable, so a later build that learns
    /// the name would fold the engine's row as that fact.
    #[error("`{0}` is not an engine event kind")]
    NotAnEngineKind(String),
    /// An unknown kind that is not a lowercase snake_case tag of at most 64 characters (it becomes
    /// part of the dedup key and the stored kind, which must stay colon-free and bounded).
    #[error("unknown kind `{0}` is not a lowercase snake_case tag of at most 64 characters")]
    InvalidKind(String),
}

/// The namespace every engine kind this build does not know must be under to be stored.
///
/// The open-enum contract lets a newer plugin send a kind an older backend does not know; the
/// backend stores it unfolded. Unscoped, that let a capability-token holder plant a row under any
/// name, including one a later release adds as a control-plane fact (an approval, a cost
/// override), which that release would then fold from the immutable log as if the control plane
/// had written it. So a NEW engine kind is named `engine_<name>` (the thirteen engine kinds that
/// predate this rule keep their names; `engine_task_verdict` is the first under it), and no
/// control-plane or AWS kind may ever take this prefix (pinned by a test on [`Kind::KNOWN`]: a
/// known kind under it must be one of [`EngineBody::KINDS`]).
pub const ENGINE_KIND_PREFIX: &str = "engine_";

/// One raw engine wire element → RunEvent, tolerating kinds this build does not know.
///
/// The ingest route parses a batch element by element through this, so one event from a newer
/// plugin cannot reject the whole batch (the open-enum contract):
/// - a known engine kind ([`EngineBody::KINDS`]) is parsed strictly and translated by
///   [`from_engine`]; a malformed body is an error;
/// - a kind that is a known RunEvent [`Kind`] but not an engine kind is an error;
/// - an unknown kind outside the engine namespace ([`ENGINE_KIND_PREFIX`]) is an error;
/// - any other kind (`engine_*`) is stored as [`EventData::Unknown`] (not folded), attributed to
///   the element's `run_id` (which the caller still checks against its capability) and keyed on a
///   hash of the whole element, so a redelivery dedupes.
pub fn from_engine_value(
    source: EventSource,
    value: serde_json::Value,
) -> Result<RunEvent, EngineEventError> {
    let kind = value
        .get("kind")
        .and_then(|k| k.as_str())
        .ok_or(EngineEventError::MissingKind)?
        .to_string();
    if EngineBody::KINDS.contains(&kind.as_str()) {
        let ev: EngineEvent = serde_json::from_value(value)
            .map_err(|error| EngineEventError::Malformed { kind, error })?;
        return Ok(from_engine(source, &ev));
    }
    if Kind::from_tag(&kind).is_known() {
        return Err(EngineEventError::NotAnEngineKind(kind));
    }
    let valid_tag = kind.len() <= 64
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid_tag {
        return Err(EngineEventError::InvalidKind(kind));
    }
    if !kind.starts_with(ENGINE_KIND_PREFIX) {
        return Err(EngineEventError::NotAnEngineKind(kind));
    }
    /// The envelope every engine event carries, whatever its kind.
    #[derive(Deserialize)]
    struct Envelope {
        ts: Timestamp,
        run_id: RunId,
    }
    // Read from the borrowed value: only the two fields are needed, and `value` is stored whole.
    let envelope = Envelope::deserialize(&value)
        .map_err(|error| EngineEventError::Malformed { kind: kind.clone(), error })?;
    let dedup_key = dedup::engine(
        source.as_str(),
        &envelope.run_id,
        None,
        None,
        &kind,
        Some(&content_disc(&value)),
    );
    Ok(RunEvent {
        run_id: Some(envelope.run_id),
        ..RunEvent::new(dedup_key, envelope.ts, source, EventData::unknown(kind, value))
    })
}

/// The dedup discriminator of a `task_outputs` batch: a hash of its role, reuse flag and sorted,
/// de-duplicated URI set (16 hex chars). An output's `path`, `etag` and `size` are left out: the
/// URIs identify the fact, so a re-report of the same URIs with other metadata is a redelivery
/// (the read side fills `etag` and `size` from S3).
fn task_outputs_disc(
    role: Option<&crate::event::OutputRole>,
    reused: bool,
    outputs: &[crate::event::OutputRef],
) -> String {
    let uris: std::collections::BTreeSet<&str> =
        outputs.iter().map(|o| o.s3_uri.as_str()).collect();
    // Length-prefixed (see `parts_disc`): an S3 key may hold a line break.
    let head = [role.map_or("output", |r| r.as_str()), if reused { "1" } else { "0" }];
    parts_disc(head.into_iter().chain(uris))
}

/// The dedup discriminator of a `run_dag`: a hash of its sorted, de-duplicated node and edge sets
/// (16 hex chars), so emission order does not matter.
fn run_dag_disc(dag: &crate::event::Dag) -> String {
    let nodes: std::collections::BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
    let edges: std::collections::BTreeSet<(&str, &str)> =
        dag.edges.iter().map(|e| (e.from.as_str(), e.to.as_str())).collect();
    parts_disc(
        std::iter::once("nodes")
            .chain(nodes)
            .chain(std::iter::once("edges"))
            .chain(edges.into_iter().flat_map(|(from, to)| [from, to])),
    )
}

/// A SHA-256 of `parts` (16 hex chars), each part length-prefixed so none can forge another's
/// boundary.
fn parts_disc<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    hex::encode(h.finalize())[..16].to_string()
}

/// The dedup discriminator of an unknown-kind engine event: a hash of its canonical JSON (object
/// keys sorted at every level), 16 hex chars — stable however the producer orders its keys.
fn content_disc(value: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String(k.clone()).to_string());
                    out.push(':');
                    canonical(&map[k], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }
    let mut s = String::new();
    canonical(value, &mut s);
    hex::encode(Sha256::digest(s.as_bytes()))[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_reason_keeps_the_container_s_explanation() {
        assert_eq!(
            batch_reason(Some("Task failed to start"), Some("CannotPullContainerError: gone")),
            Some("Task failed to start: CannotPullContainerError: gone".into())
        );
        assert_eq!(
            batch_reason(Some("Essential container exited"), None),
            Some("Essential container exited".into())
        );
        assert_eq!(batch_reason(None, Some("OutOfMemoryError")), Some("OutOfMemoryError".into()));
        assert_eq!(batch_reason(Some("same"), Some("same")), Some("same".into()));
        assert_eq!(batch_reason(Some(" "), None), None);
    }

    #[test]
    fn category_matches_words_and_markers_not_substrings() {
        use TerminationCategory as TC;
        let cat = |r: &str| category_from_reason(Some(r));
        // The markers Batch/ECS actually use.
        assert_eq!(cat("OutOfMemoryError: Container killed due to memory usage"), Some(TC::Oom));
        assert_eq!(cat("OutOfMemory: killed"), Some(TC::Oom));
        assert_eq!(cat("container OOMKilled"), Some(TC::Oom));
        assert_eq!(cat("process ran out of memory"), Some(TC::Oom));
        assert_eq!(cat("Host EC2 (instance i-0abc) terminated."), Some(TC::HostTerminated));
        assert_eq!(cat("some spot reclaim"), Some(TC::Spot));
        // Substrings of unrelated words are not markers.
        assert_eq!(cat("Essential container in task exited: room-service"), None);
        assert_eq!(cat("Essential container exited: zoom"), None);
        assert_eq!(cat("Essential container exited: hotspot-caller failed"), None);
        assert_eq!(cat("requested memory exceeds the instance's"), None);
        // A container that never started quotes its image; nothing in it describes the ending.
        assert_eq!(
            cat("Task failed to start: CannotPullContainerError: pull ecr/spot/bloom-oom:1"),
            None
        );
        assert_eq!(category_from_reason(None), None);
    }

    #[test]
    fn a_dag_disc_ignores_order_and_duplicates_but_not_content() {
        use crate::event::{Dag, DagEdge, DagNode};
        let node = |id: &str| DagNode { id: id.into() };
        let edge = |from: &str, to: &str| DagEdge { from: from.into(), to: to.into() };
        let a = Dag { nodes: vec![node("a"), node("b")], edges: vec![edge("a", "b")] };
        let reordered = Dag {
            nodes: vec![node("b"), node("a"), node("a")],
            edges: vec![edge("a", "b"), edge("a", "b")],
        };
        let grown = Dag {
            nodes: vec![node("a"), node("b"), node("c")],
            edges: vec![edge("a", "b"), edge("b", "c")],
        };
        assert_eq!(run_dag_disc(&a), run_dag_disc(&reordered));
        assert_ne!(run_dag_disc(&a), run_dag_disc(&grown));
        // A node id cannot pose as an edge: the parts are length-prefixed.
        let tricky = Dag { nodes: vec![node("a"), node("b"), node("edges")], edges: vec![] };
        assert_ne!(run_dag_disc(&a), run_dag_disc(&tricky));
    }

    #[test]
    fn a_content_disc_ignores_key_order() {
        let a = serde_json::json!({"kind": "k", "x": {"b": 1, "a": [1, {"d": 2, "c": 3}]}});
        let b: serde_json::Value =
            serde_json::from_str(r#"{"x": {"a": [1, {"c": 3, "d": 2}], "b": 1}, "kind": "k"}"#)
                .unwrap();
        assert_eq!(content_disc(&a), content_disc(&b));
        let c = serde_json::json!({"kind": "k", "x": {"b": 2, "a": [1, {"d": 2, "c": 3}]}});
        assert_ne!(content_disc(&a), content_disc(&c));
    }
}
