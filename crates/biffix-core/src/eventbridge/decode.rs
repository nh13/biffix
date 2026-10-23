//! Decode raw AWS/EventBridge events into normalized RunEvents (and ECS host facts).
//!
//! The translator is deliberately dumb: decode the EventBridge `detail` into the
//! matching per-source wire type, then delegate to [`crate::translate`]. Worker vs
//! coordinator is decided here from the job's queue (the one piece of context the raw event
//! doesn't carry as a boolean).
//!
//! The ingest reconciler reads the same facts from the AWS SDK's DescribeJobs
//! ([`super::describe`]); it shares [`queue_name`], [`container_instance_id`] and
//! [`requested_resources`] with these decoders so both record a job identically.

use crate::event::RunEvent;
use crate::ids::{BatchJobId, InstanceId};
use crate::time::Timestamp;
use crate::translate;
use crate::wire::aws_batch::{AwsBatchEvent, BatchStatus};
use crate::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus, SUBJECT_VARIABLE};
use crate::wire::aws_ec2::{Ec2Event, Ec2EventKind};

/// Why an event `route` matched by its `source` and `detail-type` could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("malformed detail: {0}")]
    Malformed(String),
}

/// The envelope's `detail`; an event routed by its `source` and `detail-type` without one is
/// malformed.
fn detail_of(event: &serde_json::Value) -> Result<&serde_json::Value, DecodeError> {
    event.get("detail").ok_or_else(|| DecodeError::Malformed("missing detail".into()))
}

/// The envelope's `time`, when present and parseable.
fn envelope_time(event: &serde_json::Value) -> Option<Timestamp> {
    event
        .get("time")
        .and_then(|v| v.as_str())
        .and_then(parse_rfc3339_secs)
        .map(Timestamp::from_epoch_seconds)
}

/// The envelope's `time`, which an event without a source time of its own requires.
fn required_envelope_time(event: &serde_json::Value) -> Result<Timestamp, DecodeError> {
    envelope_time(event)
        .ok_or_else(|| DecodeError::Malformed("missing or unparseable `time`".into()))
}

/// Which queues carry the coordinator (head) job, so a Batch event can be classified.
pub struct QueueContext {
    pub coordinator_queues: Vec<String>,
}

/// A Batch job queue's name from its ARN (`arn:aws:batch:…:job-queue/<name>`); a bare name is
/// returned unchanged.
pub fn queue_name(queue_arn: &str) -> &str {
    queue_arn.rsplit([':', '/']).next().unwrap_or(queue_arn)
}

/// The container-instance id in a `containerInstanceArn`: its last `/` segment (the whole string
/// when it has none). Batch job events and ECS host facts are keyed by it, so they join.
pub fn container_instance_id(container_instance_arn: &str) -> &str {
    container_instance_arn.rsplit('/').next().unwrap_or(container_instance_arn)
}

impl QueueContext {
    pub fn is_coordinator(&self, queue_arn: &str) -> bool {
        // Match the full ARN or its final segment (the queue name), not a bare `ends_with`, so a
        // configured name like "coordinator" cannot accidentally match an unrelated queue whose
        // ARN merely ends with that substring (e.g. "…/my-coordinator-shadow").
        let name = queue_name(queue_arn);
        self.coordinator_queues.iter().any(|q| queue_arn == q || name == q)
    }
}

fn parse_status(s: &str) -> BatchStatus {
    // Single-source the string→status mapping through BatchStatus's own `Deserialize`
    // (SCREAMING_SNAKE_CASE + untagged `Unknown` fallback) rather than a parallel hand-rolled
    // table a new variant could silently desync from. The untagged arm makes this infallible.
    use serde::de::value::{Error, StrDeserializer};
    serde::Deserialize::deserialize(StrDeserializer::<Error>::new(s))
        .unwrap_or_else(|_| BatchStatus::Unknown(s.to_string()))
}

/// Decode an EventBridge "Batch Job State Change" event (the whole envelope) → RunEvent.
pub fn decode_batch(
    event: &serde_json::Value,
    ctx: &QueueContext,
) -> Result<RunEvent, DecodeError> {
    let detail = detail_of(event)?;
    let job_id = detail
        .get("jobId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DecodeError::Malformed("missing jobId".into()))?;
    let status = detail
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DecodeError::Malformed("missing status".into()))?;
    let queue = detail.get("jobQueue").and_then(|v| v.as_str()).unwrap_or("");

    // The event's source time: Batch's own status timestamp when present, else the envelope `time`
    // as a strict fallback (never a fabricated epoch zero). This feeds the fold's source-time
    // fields (started/finished/submitted at); it no longer affects the dedup key, which is keyed on
    // `(job, status)` alone (see `dedup::aws_batch`), so a missing `stoppedAt` here can no longer
    // desync the live and reconciled terminal into two keys.
    let status_ts_millis = match status {
        "SUCCEEDED" | "FAILED" => detail.get("stoppedAt").and_then(serde_json::Value::as_i64),
        "STARTING" | "RUNNING" => detail.get("startedAt").and_then(serde_json::Value::as_i64),
        _ => detail.get("createdAt").and_then(serde_json::Value::as_i64),
    };
    let ts = status_ts_millis
        .map(Timestamp::from_epoch_millis)
        .or_else(|| envelope_time(event))
        .ok_or_else(|| {
            DecodeError::Malformed(
                "missing status timestamp and missing or unparseable `time`".into(),
            )
        })?;

    let container = detail.get("container");
    let exit_code = container
        .and_then(|c| c.get("exitCode"))
        .and_then(serde_json::Value::as_i64)
        .map(|n| n as i32);
    let log_stream =
        container.and_then(|c| c.get("logStreamName")).and_then(|v| v.as_str()).map(String::from);
    let status_reason = translate::batch_reason(
        detail.get("statusReason").and_then(|v| v.as_str()),
        container.and_then(|c| c.get("reason")).and_then(|v| v.as_str()),
    );
    // The Batch event has no raw EC2 instance id; the container-instance ARN is the only host
    // identifier it carries. Keep just the id (the ARN's last segment) — enough to identify/group
    // by host in the UI without an ECS DescribeContainerInstances round-trip in the hot path.
    let instance_id = container
        .and_then(|c| c.get("containerInstanceArn"))
        .and_then(|v| v.as_str())
        .map(|arn| InstanceId::new(container_instance_id(arn)));

    let requested = requested_resources_from_json(container);

    let wire = AwsBatchEvent {
        ts,
        batch_job_id: BatchJobId::new(job_id),
        status: parse_status(status),
        exit_code,
        status_reason,
        log_stream,
        instance_id,
        queue: Some(queue_name(queue)).filter(|name| !name.is_empty()).map(String::from),
        is_coordinator: ctx.is_coordinator(queue),
        // Attempts Batch already finished for this job id (a retryStrategy re-run keeps the id):
        // tells a retry's RUNNABLE/STARTING/RUNNING from the first attempt's in the dedup key.
        prior_attempts: detail
            .get("attempts")
            .and_then(|a| a.as_array())
            .and_then(|a| u32::try_from(a.len()).ok()),
        requested_vcpus: requested.vcpus,
        requested_memory_mib: requested.memory_mib,
    };
    Ok(translate::from_aws_batch(&wire))
}

/// The resources a Batch job asked for (not what its host has): vCPUs and memory in MiB.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RequestedResources {
    pub vcpus: Option<f64>,
    pub memory_mib: Option<i64>,
}

/// The resources a Batch job requested, from its container's `resourceRequirements`
/// (`[{type: VCPU|MEMORY|GPU, value: "<number>"}]`, MEMORY in MiB) — the shape Batch reports for
/// every job registered with them, which is how both engines' executors submit — falling back to
/// the legacy `vcpus` / `memory` fields per resource when no usable requirement names it. A value
/// is usable only if it is still a positive number after conversion (memory is rounded to whole
/// MiB and must fit an `i64`); anything else is ignored, never guessed. Among duplicate
/// requirements for one resource the first usable one wins. Shared by the EventBridge decoder and
/// DescribeJobs reconciliation, which see the same `container` block, so both record the same
/// request.
pub fn requested_resources<'a, I, V>(
    requirements: I,
    legacy_vcpus: Option<f64>,
    legacy_memory_mib: Option<f64>,
) -> RequestedResources
where
    I: IntoIterator<Item = (&'a str, V)>,
    V: AsRef<str>,
{
    let mut requested = RequestedResources::default();
    for (kind, value) in requirements {
        let Ok(value) = value.as_ref().trim().parse::<f64>() else {
            continue;
        };
        match kind {
            "VCPU" if requested.vcpus.is_none() => requested.vcpus = vcpus_from(value),
            "MEMORY" if requested.memory_mib.is_none() => {
                requested.memory_mib = memory_mib_from(value);
            }
            _ => {}
        }
    }
    RequestedResources {
        vcpus: requested.vcpus.or_else(|| legacy_vcpus.and_then(vcpus_from)),
        memory_mib: requested.memory_mib.or_else(|| legacy_memory_mib.and_then(memory_mib_from)),
    }
}

/// A vCPU request: `value` when it is a finite number above zero.
fn vcpus_from(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// A memory request in whole MiB: `value` rounded, when that is at least 1 MiB and fits an `i64`
/// (checked before the cast, which would otherwise saturate a huge value to `i64::MAX`).
fn memory_mib_from(value: f64) -> Option<i64> {
    let mib = value.round();
    // `i64::MAX as f64` is 2^63, one past the largest i64, so `<` keeps the cast exact.
    (mib.is_finite() && mib >= 1.0 && mib < i64::MAX as f64).then_some(mib as i64)
}

/// [`requested_resources`] of an EventBridge Batch event's `detail.container` (absent ⇒ none, as
/// for a multi-node or ECS-properties job, whose requirements live elsewhere). A requirement's
/// `value` is a string in what Batch emits; a JSON number is accepted too rather than dropped.
fn requested_resources_from_json(container: Option<&serde_json::Value>) -> RequestedResources {
    let Some(container) = container else {
        return RequestedResources::default();
    };
    let requirements = container
        .get("resourceRequirements")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|requirement| {
            let kind = requirement.get("type")?.as_str()?;
            let value = match requirement.get("value")? {
                serde_json::Value::String(value) => std::borrow::Cow::Borrowed(value.as_str()),
                serde_json::Value::Number(value) => std::borrow::Cow::Owned(value.to_string()),
                _ => return None,
            };
            Some((kind, value))
        });
    requested_resources(
        requirements,
        container.get("vcpus").and_then(serde_json::Value::as_f64),
        container.get("memory").and_then(serde_json::Value::as_f64),
    )
}

/// Decode an EventBridge "EC2 Spot Instance Interruption Warning" → RunEvent.
pub fn decode_spot(event: &serde_json::Value) -> Result<RunEvent, DecodeError> {
    let detail = detail_of(event)?;
    let instance = detail
        .get("instance-id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DecodeError::Malformed("missing instance-id".into()))?;
    let ts = required_envelope_time(event)?;
    let wire = Ec2Event {
        ts,
        instance_id: InstanceId::new(instance),
        kind: Ec2EventKind::SpotInterruptionWarning,
    };
    Ok(translate::from_aws_ec2(&wire))
}

/// A CodeBuild "Build State Change" event → RunEvent, or `Ok(None)` for a build biffix did not
/// start (one without a `BIFFIX_BUILD_SUBJECT` environment variable).
///
/// Every state change carries the build's environment variables (so the subject); a finished build
/// also carries its exported variables (its outputs) and, when it failed, the failed phase.
pub fn decode_codebuild(event: &serde_json::Value) -> Result<Option<RunEvent>, DecodeError> {
    let detail = detail_of(event)?;
    let info = detail.get("additional-information");
    let variables = |list: &str| {
        info.and_then(|i| {
            let path = if list == "environment-variables" {
                i.get("environment").and_then(|e| e.get(list))
            } else {
                i.get(list)
            };
            path.and_then(|v| v.as_array())
        })
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v.get("name")?.as_str()?, v.get("value")?.as_str()?)))
    };
    let Some(subject) = variables("environment-variables")
        .find_map(|(name, value)| (name == SUBJECT_VARIABLE).then(|| value.to_string()))
    else {
        return Ok(None);
    };
    let field = |name: &str| {
        detail
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| DecodeError::Malformed(format!("missing {name}")))
    };
    let status: CodeBuildStatus =
        serde_json::from_value(serde_json::Value::String(field("build-status")?))
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let ts = required_envelope_time(event)?;
    // Each `phase-context` entry is already a `STATUS: message` string.
    let reason = failed_phase_reason(
        info.and_then(|i| i.get("phases")).and_then(|p| p.as_array()).into_iter().flatten().map(
            |phase| {
                let field = |key: &str| phase.get(key).and_then(|v| v.as_str());
                let context = phase
                    .get("phase-context")
                    .and_then(|c| c.as_array())
                    .map(|c| c.iter().filter_map(|m| m.as_str()).map(String::from).collect())
                    .unwrap_or_default();
                (field("phase-status"), field("phase-type"), context)
            },
        ),
    );
    let wire = CodeBuildEvent {
        ts,
        build_arn: field("build-id")?,
        // Nothing reads it, so a build state change without one is still the build's.
        project: field("project-name").unwrap_or_default(),
        status,
        subject,
        outputs: variables("exported-environment-variables")
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        reason,
        logs_url: info
            .and_then(|i| i.get("logs"))
            .and_then(|l| l.get("deep-link"))
            .and_then(|d| d.as_str())
            .map(str::to_string),
    };
    Ok(Some(translate::from_aws_codebuild(&wire)))
}

/// Why a CodeBuild build failed: its first phase that did not succeed, with its context messages
/// joined (`BUILD: COMMAND_EXECUTION_ERROR: …`), or `None` when no reported phase failed. Each
/// phase is `(phase-status, phase-type, contexts)`; a phase without a type is named `?`. Shared by
/// [`decode_codebuild`] and the backend's BatchGetBuilds poll, so a polled build and its state
/// change event give the same reason.
pub fn failed_phase_reason<'a>(
    phases: impl IntoIterator<Item = (Option<&'a str>, Option<&'a str>, Vec<String>)>,
) -> Option<String> {
    let (_, name, context) =
        phases.into_iter().find(|(status, _, _)| status.is_some_and(|s| s != "SUCCEEDED"))?;
    let name = name.unwrap_or("?");
    Some(if context.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {}", context.join("; "))
    })
}

/// Host facts from one ECS "Container Instance State Change" event, destined for `host_registry`.
///
/// Not a [`RunEvent`]: these are infrastructure facts, invariant for a container instance's
/// lifetime (a re-registration gets a new id), recorded as observed and joined to attempts at read
/// time via the container-instance id Batch job events already carry (see [`decode_batch`]).
#[derive(Debug, Clone, PartialEq)]
pub struct HostFact {
    /// The last segment of `containerInstanceArn` — the same rule [`decode_batch`] applies, so the
    /// two sides join on an identical key.
    pub container_instance_id: String,
    pub ec2_instance_id: String,
    pub cluster_arn: String,
    pub instance_type: Option<String>,
    pub availability_zone: Option<String>,
    pub cpu_architecture: Option<String>,
    pub os_family: Option<String>,
    pub ami_id: Option<String>,
    pub subnet_id: Option<String>,
    /// ECS-allocatable vCPUs (registered CPU units / 1024).
    pub registered_vcpus: Option<f32>,
    /// ECS-allocatable memory in MiB (physical memory minus the agent's reservation).
    pub registered_memory_mib: Option<i64>,
    /// Number of GPU ids ECS registered; `None` when the host reports no GPU resource.
    pub gpu_count: Option<i32>,
    pub agent_version: Option<String>,
    pub docker_version: Option<String>,
    /// `detail.version`: ECS increments it on every state change of the container instance.
    pub detail_version: i64,
    /// The event's `detail`, verbatim, so fields not modeled here are never lost.
    pub raw_detail: serde_json::Value,
    /// The event's source time, epoch micros: the first of `detail.updatedAt`,
    /// `detail.registeredAt` and the envelope `time` that parses.
    pub observed_at_micros: i64,
}

/// Decode an EventBridge "ECS Container Instance State Change" event (the whole envelope).
///
/// The ids and cluster are required; every descriptive field is optional because ECS fills some in
/// only on later events (e.g. `versionInfo` is empty on the first registration event).
pub fn decode_ecs_host(event: &serde_json::Value) -> Result<HostFact, DecodeError> {
    let detail = detail_of(event)?;
    let required = |key: &str| {
        detail
            .get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| DecodeError::Malformed(format!("missing {key}")))
    };
    let container_instance_arn = required("containerInstanceArn")?;
    let ec2_instance_id = required("ec2InstanceId")?;
    let cluster_arn = required("clusterArn")?;
    let detail_version = detail
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| DecodeError::Malformed("missing version".into()))?;

    // The first candidate that is present AND parses: an odd `updatedAt` falls through to
    // `registeredAt`, then the envelope time, rather than dropping the whole host fact.
    let observed_at_secs = [detail.get("updatedAt"), detail.get("registeredAt"), event.get("time")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find_map(parse_rfc3339_secs)
        .ok_or_else(|| DecodeError::Malformed("missing or unparseable timestamp".into()))?;

    let attribute = |name: &str| {
        detail
            .get("attributes")
            .and_then(|v| v.as_array())
            .and_then(|attrs| {
                attrs.iter().find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
            })
            .and_then(|a| a.get("value"))
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    let resource = |name: &str| {
        detail.get("registeredResources").and_then(|v| v.as_array()).and_then(|resources| {
            resources.iter().find(|r| r.get("name").and_then(|n| n.as_str()) == Some(name))
        })
    };
    let integer_resource = |name: &str| {
        resource(name).and_then(|r| r.get("integerValue")).and_then(serde_json::Value::as_i64)
    };
    let version_info = |key: &str| {
        detail
            .get("versionInfo")
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str())
            .map(String::from)
    };

    Ok(HostFact {
        container_instance_id: container_instance_id(&container_instance_arn).to_string(),
        ec2_instance_id,
        cluster_arn,
        instance_type: attribute("ecs.instance-type"),
        availability_zone: attribute("ecs.availability-zone"),
        cpu_architecture: attribute("ecs.cpu-architecture"),
        os_family: attribute("ecs.os-family"),
        ami_id: attribute("ecs.ami-id"),
        subnet_id: attribute("ecs.subnet-id"),
        registered_vcpus: integer_resource("CPU").map(|units| units as f32 / 1024.0),
        registered_memory_mib: integer_resource("MEMORY"),
        gpu_count: resource("GPU")
            .and_then(|r| r.get("stringSetValue"))
            .and_then(|v| v.as_array())
            .map(|ids| ids.len() as i32),
        agent_version: version_info("agentVersion"),
        docker_version: version_info("dockerVersion"),
        detail_version,
        raw_detail: detail.clone(),
        observed_at_micros: Timestamp::from_epoch_seconds(observed_at_secs).as_micros(),
    })
}

/// RFC 3339 → epoch seconds (fractional part kept), or `None` for anything that is not a valid
/// RFC 3339 timestamp — including impossible calendar dates such as Feb 31, which are rejected, not
/// rolled into the next month. Any UTC offset is accepted and normalized. Never panics: an
/// out-of-range input is a `None`, so one bad timestamp cannot unwind an SQS batch.
fn parse_rfc3339_secs(s: &str) -> Option<f64> {
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_nanos()) / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(queues: &[&str]) -> QueueContext {
        QueueContext {
            coordinator_queues: queues.iter().map(std::string::ToString::to_string).collect(),
        }
    }

    #[test]
    fn is_coordinator_matches_full_arn_or_final_segment_not_a_substring() {
        let c = ctx(&["coordinator"]);
        // Exact final-segment match, and the full ARN, are coordinators.
        assert!(c.is_coordinator("arn:aws:batch:us-east-1:1:job-queue/coordinator"));
        assert!(ctx(&["arn:aws:batch:us-east-1:1:job-queue/coordinator"])
            .is_coordinator("arn:aws:batch:us-east-1:1:job-queue/coordinator"));
        // A different segment that merely ENDS WITH the configured name must NOT match — this is
        // the exact regression a bare `ends_with` would introduce (`…-coordinator`.ends_with =
        // true).
        assert!(!c.is_coordinator("arn:aws:batch:us-east-1:1:job-queue/biffix-dev-coordinator"));
        // …nor one that merely CONTAINS it (what a `contains` would wrongly match).
        assert!(!c.is_coordinator("arn:aws:batch:us-east-1:1:job-queue/coordinator-shadow"));
        // A clearly unrelated worker queue is not a coordinator.
        assert!(!c.is_coordinator("arn:aws:batch:us-east-1:1:job-queue/biffix-dev-cpu-spot"));
    }

    #[test]
    fn parse_rfc3339_pins_known_epoch_values() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:00Z"), Some(0.0));
        // The canonical 1e9 and 1234567890 timestamps.
        assert_eq!(parse_rfc3339_secs("2001-09-09T01:46:40Z"), Some(1_000_000_000.0));
        assert_eq!(parse_rfc3339_secs("2009-02-13T23:31:30Z"), Some(1_234_567_890.0));
        // Fractional seconds are preserved.
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:01.5Z"), Some(1.5));
        assert_eq!(parse_rfc3339_secs("2009-02-13T23:31:30.250Z"), Some(1_234_567_890.25));
        // A numeric offset is normalized to UTC; lowercase separators are RFC 3339 too.
        assert_eq!(parse_rfc3339_secs("2009-02-14T01:31:30+02:00"), Some(1_234_567_890.0));
        assert_eq!(parse_rfc3339_secs("2009-02-13T18:31:30-05:00"), Some(1_234_567_890.0));
        assert_eq!(parse_rfc3339_secs("2009-02-13t23:31:30z"), Some(1_234_567_890.0));
        // Pre-1970 is negative, not a panic.
        assert_eq!(parse_rfc3339_secs("1969-12-31T23:59:59Z"), Some(-1.0));
        // Leap day in a leap year; the last second of a year.
        assert_eq!(parse_rfc3339_secs("2024-02-29T00:00:00Z"), Some(1_709_164_800.0));
        assert_eq!(parse_rfc3339_secs("2025-12-31T23:59:59Z"), Some(1_767_225_599.0));
    }

    #[test]
    fn parse_rfc3339_rejects_impossible_calendar_dates() {
        // Days a month does not have are refused, not rolled into the next month.
        assert_eq!(parse_rfc3339_secs("2026-02-31T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2026-04-31T00:00:00Z"), None);
        // Feb 29 only exists in leap years.
        assert_eq!(parse_rfc3339_secs("2025-02-29T00:00:00Z"), None);
        // No offset is not RFC 3339.
        assert_eq!(parse_rfc3339_secs("2026-01-01T00:00:00"), None);
        assert_eq!(parse_rfc3339_secs(""), None);
    }

    #[test]
    fn parse_rfc3339_rejects_out_of_range_without_panicking() {
        // Out-of-range fields (a many-digit year, month 13, day 32) are refused (None), never a
        // panic that would unwind the SQS batch.
        assert_eq!(parse_rfc3339_secs("999999999999999-01-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2026-13-01T00:00:00Z"), None); // month 13
        assert_eq!(parse_rfc3339_secs("2026-01-32T00:00:00Z"), None); // day 32
        assert_eq!(parse_rfc3339_secs("2026-01-01T24:00:00Z"), None); // hour 24
        assert_eq!(parse_rfc3339_secs("not-a-timestamp"), None);
    }

    #[test]
    fn a_requested_value_is_kept_only_if_still_positive_after_conversion() {
        let requested = |vcpu: &str, memory: &str| {
            let r = requested_resources([("VCPU", vcpu), ("MEMORY", memory)], None, None);
            (r.vcpus, r.memory_mib)
        };
        assert_eq!(requested("0.25", "512.4"), (Some(0.25), Some(512)));
        // Memory that rounds to zero MiB, or past what an i64 holds (the cast would saturate), is
        // not a request; nor is a vCPU count that is not a finite positive number.
        assert_eq!(requested("1e400", "0.4"), (None, None));
        assert_eq!(requested("-1", "1e39"), (None, None));
        assert_eq!(requested("NaN", "inf"), (None, None));
        assert_eq!(requested("1e-50", "0.5"), (Some(1e-50), Some(1)));
        // The largest exact f64 below 2^63 still fits.
        assert_eq!(requested("1", "9223372036854774784").1, Some(9_223_372_036_854_774_784));
        assert_eq!(requested("1", "9223372036854775808").1, None);
        // The legacy fallback applies the same rule.
        let legacy = requested_resources::<[(&str, &str); 0], &str>([], Some(0.0), Some(0.4));
        assert_eq!(legacy, RequestedResources::default());
    }

    #[test]
    fn an_unusable_requirement_falls_back_to_legacy_and_the_first_usable_duplicate_wins() {
        let r = requested_resources(
            [("VCPU", "lots"), ("VCPU", "2"), ("VCPU", "8"), ("MEMORY", "0")],
            Some(16.0),
            Some(4096.0),
        );
        assert_eq!(r.vcpus, Some(2.0), "the first usable VCPU, not the legacy field");
        assert_eq!(r.memory_mib, Some(4096), "an unusable MEMORY falls back to legacy");
    }

    #[test]
    fn the_failed_phase_reason_is_the_first_unsucceeded_phase_with_its_contexts() {
        let ok = |name| (Some("SUCCEEDED"), Some(name), vec![]);
        assert_eq!(failed_phase_reason([]), None);
        assert_eq!(failed_phase_reason([ok("INSTALL"), ok("BUILD")]), None);
        // A phase without a status (one still running) is not a failure.
        assert_eq!(failed_phase_reason([(None, Some("BUILD"), vec![])]), None);
        let failed = |name, context: &[&str]| {
            (Some("FAILED"), name, context.iter().map(std::string::ToString::to_string).collect())
        };
        assert_eq!(
            failed_phase_reason([
                ok("INSTALL"),
                failed(Some("BUILD"), &["COMMAND_EXECUTION_ERROR: exit 1", "x: y"]),
                failed(Some("POST_BUILD"), &[]),
            ])
            .as_deref(),
            Some("BUILD: COMMAND_EXECUTION_ERROR: exit 1; x: y")
        );
        assert_eq!(failed_phase_reason([failed(None, &[])]).as_deref(), Some("?"));
        assert_eq!(
            failed_phase_reason([(Some("TIMED_OUT"), Some("BUILD"), vec![])]).as_deref(),
            Some("BUILD")
        );
    }

    #[test]
    fn the_host_id_is_the_arns_last_segment() {
        assert_eq!(
            container_instance_id("arn:aws:ecs:us-east-1:1:container-instance/cluster/abc123"),
            "abc123"
        );
        assert_eq!(container_instance_id("abc123"), "abc123");
    }
}
