//! Translator tests — wire event → normalized RunEvent.

use biffix_core::event::{EventData, Kind, TerminationCategory};
use biffix_core::ids::{Attempt, BatchJobId, InstanceId, RunId, TaskKey};
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use biffix_core::translate;
use biffix_core::wire::aws_batch::{AwsBatchEvent, BatchStatus};
use biffix_core::wire::aws_ec2::{Ec2Event, Ec2EventKind};
use biffix_core::wire::engine::{EngineBody, EngineEvent};

fn ts() -> Timestamp {
    Timestamp::from_epoch_seconds(1776142351.0)
}

#[test]
fn aws_batch_worker_failed_maps_to_task_failed() {
    let w = AwsBatchEvent {
        ts: ts(),
        batch_job_id: BatchJobId::new("job-1"),
        status: BatchStatus::Failed,
        exit_code: Some(1),
        status_reason: Some("some spot reclaim".into()),
        log_stream: None,
        instance_id: Some(InstanceId::new("i-1")),
        queue: None,
        is_coordinator: false,
        prior_attempts: None,
        requested_vcpus: None,
        requested_memory_mib: None,
    };
    let re = translate::from_aws_batch(&w);
    assert_eq!(re.kind(), Kind::TaskFailed);
    assert!(re.run_id.is_none()); // stitched later
    assert_eq!(re.batch_job_id.clone().unwrap().as_str(), "job-1");
    // dedup key carries the AWS *wire* status (SCREAMING_SNAKE_CASE), never Rust Debug, and is
    // keyed on (job, status) alone — timestamp-independent.
    assert_eq!(re.dedup_key, "aws-batch:job-1:FAILED");
    // the reason-derived enrichment is actually populated: "spot" ⇒ Spot.
    match re.data {
        EventData::TaskFailed { exit_code, termination_category, status_reason, .. } => {
            assert_eq!(exit_code, 1);
            assert_eq!(termination_category, Some(biffix_core::event::TerminationCategory::Spot));
            assert_eq!(status_reason.as_deref(), Some("some spot reclaim"));
        }
        other => panic!("expected TaskFailed, got {other:?}"),
    }
}

#[test]
fn aws_batch_failed_reason_classifies_oom_and_host_terminated() {
    let mk = |reason: &str| AwsBatchEvent {
        ts: ts(),
        batch_job_id: BatchJobId::new("job-1"),
        status: BatchStatus::Failed,
        exit_code: Some(137),
        status_reason: Some(reason.into()),
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: false,
        prior_attempts: None,
        requested_vcpus: None,
        requested_memory_mib: None,
    };
    let cat = |reason: &str| match translate::from_aws_batch(&mk(reason)).data {
        EventData::TaskFailed { termination_category, .. } => termination_category,
        other => panic!("expected TaskFailed, got {other:?}"),
    };
    use biffix_core::event::TerminationCategory as TC;
    assert_eq!(cat("OutOfMemory: killed"), Some(TC::Oom));
    assert_eq!(cat("Host EC2 instance terminated"), Some(TC::HostTerminated));
    assert_eq!(cat("exit code 2"), None);
    // A reason matching BOTH "spot" and "host…terminated" must classify as Spot: the translator
    // checks spot first, and this pins that precedence so a reorder cannot silently flip it.
    assert_eq!(
        cat("Host EC2 instance terminated by spot interruption"),
        Some(TC::Spot),
        "spot takes precedence when both keywords are present"
    );
}

#[test]
fn aws_batch_coordinator_succeeded_maps_to_coordinator_succeeded() {
    let w = AwsBatchEvent {
        ts: ts(),
        batch_job_id: BatchJobId::new("coord-1"),
        status: BatchStatus::Succeeded,
        exit_code: Some(0),
        status_reason: None,
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: true,
        prior_attempts: None,
        requested_vcpus: None,
        requested_memory_mib: None,
    };
    assert_eq!(translate::from_aws_batch(&w).kind(), Kind::CoordinatorSucceeded);
}

#[test]
fn aws_batch_coordinator_events_carry_the_log_stream() {
    for (status, expected) in [
        (BatchStatus::Running, Kind::CoordinatorRunning),
        (BatchStatus::Succeeded, Kind::CoordinatorSucceeded),
        (BatchStatus::Failed, Kind::CoordinatorFailed),
    ] {
        let w = AwsBatchEvent {
            ts: ts(),
            batch_job_id: BatchJobId::new("coord-1"),
            status,
            exit_code: None,
            status_reason: None,
            log_stream: Some("coord/stream".into()),
            instance_id: None,
            queue: None,
            is_coordinator: true,
            prior_attempts: None,
            requested_vcpus: None,
            requested_memory_mib: None,
        };
        let re = translate::from_aws_batch(&w);
        assert_eq!(re.kind(), expected);
        let stream = match re.data {
            EventData::CoordinatorRunning { log_stream }
            | EventData::CoordinatorSucceeded { log_stream }
            | EventData::CoordinatorFailed { log_stream, .. } => log_stream,
            other => panic!("unexpected payload {other:?}"),
        };
        assert_eq!(stream.as_deref(), Some("coord/stream"));
    }
}

#[test]
fn ec2_spot_warning_maps_to_spot_reclaim() {
    let w = Ec2Event {
        ts: ts(),
        instance_id: InstanceId::new("i-1"),
        kind: Ec2EventKind::SpotInterruptionWarning,
    };
    assert_eq!(translate::from_aws_ec2(&w).kind(), Kind::SpotReclaimWarning);
}

#[test]
fn snakemake_task_submitted_carries_binding_fields() {
    let w = EngineEvent {
        schema_version: 1,
        ts: ts(),
        run_id: RunId::new("r1"),
        body: EngineBody::TaskSubmitted {
            task_key: TaskKey::new("align:sample=A"),
            attempt: Attempt(1),
            batch_job_id: BatchJobId::new("job-1"),
            image: None,
        },
    };
    let re = translate::from_engine(EventSource::Snakemake, &w);
    assert_eq!(re.kind(), Kind::TaskSubmitted);
    assert_eq!(re.run_id.unwrap().as_str(), "r1");
    assert_eq!(re.task_key.unwrap().as_str(), "align:sample=A");
    assert_eq!(re.batch_job_id.unwrap().as_str(), "job-1");
    assert_eq!(re.attempt.unwrap(), Attempt(1));
}

#[test]
fn a_task_submitted_image_rides_through_and_is_optional() {
    let element = serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": "task_submitted",
        "task_key": "align:A", "attempt": 1, "batch_job_id": "job-1",
        "image": "123.dkr.ecr.us-east-1.amazonaws.com/biffix/bwa@sha256:ab"});
    let re = translate::from_engine_value(EventSource::Nextflow, element.clone()).unwrap();
    assert_eq!(
        re.data,
        EventData::TaskSubmitted {
            image: Some("123.dkr.ecr.us-east-1.amazonaws.com/biffix/bwa@sha256:ab".into())
        }
    );
    // The image is not part of the fact's identity: the same submit without it is a redelivery.
    let mut bare = element;
    bare.as_object_mut().unwrap().remove("image");
    let without = translate::from_engine_value(EventSource::Nextflow, bare).unwrap();
    assert_eq!(without.data, EventData::TaskSubmitted { image: None });
    assert_eq!(without.dedup_key, re.dedup_key);
}

#[test]
fn an_engine_task_verdict_translates_and_dedupes_per_attempt() {
    use biffix_core::event::{TaskVerdict, TerminationCategory};
    let element = |attempt: u32, reason: &str| {
        serde_json::json!({"schema_version": 1, "ts": 7.0, "run_id": "r1",
            "kind": "engine_task_verdict", "task_key": "ALIGN (A)", "attempt": attempt,
            "verdict": "failed", "category": "missing_output", "reason": reason})
    };
    let re = translate::from_engine_value(EventSource::Nextflow, element(1, "no out.bam")).unwrap();
    assert_eq!(re.kind(), Kind::EngineTaskVerdict);
    assert_eq!(re.kind().as_str(), "engine_task_verdict");
    assert_eq!(re.source, EventSource::Nextflow);
    assert_eq!(re.task_key.as_ref().unwrap().as_str(), "ALIGN (A)");
    assert_eq!(re.attempt, Some(Attempt(1)));
    assert!(re.batch_job_id.is_none());
    assert_eq!(
        re.data,
        EventData::EngineTaskVerdict {
            verdict: TaskVerdict::Failed,
            category: Some(TerminationCategory::MissingOutput),
            reason: Some("no out.bam".into()),
        }
    );
    assert_eq!(re.dedup_key, "nextflow:r1:ALIGN (A):1:engine_task_verdict:failed");
    // A redelivery (even reworded) is the same fact; another attempt's verdict is not.
    let again =
        translate::from_engine_value(EventSource::Nextflow, element(1, "reworded")).unwrap();
    assert_eq!(again.dedup_key, re.dedup_key);
    let next =
        translate::from_engine_value(EventSource::Nextflow, element(2, "no out.bam")).unwrap();
    assert_ne!(next.dedup_key, re.dedup_key);
    // Only `task_key` and `verdict` are required; a body without the verdict is malformed.
    let minimal = serde_json::json!({"ts": 7.0, "run_id": "r1", "kind": "engine_task_verdict",
        "task_key": "t", "verdict": "failed"});
    let re = translate::from_engine_value(EventSource::Snakemake, minimal.clone()).unwrap();
    assert_eq!(re.attempt, None);
    // Without an attempt the verdict covers what was submitted by its ts, so a later one (which
    // covers later attempts) is a distinct fact; a redelivery still dedupes.
    assert_eq!(re.dedup_key, "snakemake:r1:t::engine_task_verdict:failed:7000000");
    let mut later = minimal.clone();
    later["ts"] = serde_json::json!(9.0);
    let later = translate::from_engine_value(EventSource::Snakemake, later).unwrap();
    assert_ne!(later.dedup_key, re.dedup_key);
    let again = translate::from_engine_value(EventSource::Snakemake, minimal).unwrap();
    assert_eq!(again.dedup_key, re.dedup_key);
    let malformed = serde_json::json!({"ts": 7.0, "run_id": "r1", "kind": "engine_task_verdict", "task_key": "t"});
    assert!(matches!(
        translate::from_engine_value(EventSource::Nextflow, malformed),
        Err(translate::EngineEventError::Malformed { .. })
    ));
}

#[test]
fn snakemake_local_task_terminal_carries_no_batch_binding() {
    // A local rule's engine terminal maps to an attribution-carrying RunEvent (run_id + task_key)
    // with NO batch_job_id/attempt, so the reducer records it as a task-level engine terminal.
    let w = EngineEvent {
        schema_version: 1,
        ts: ts(),
        run_id: RunId::new("r1"),
        body: EngineBody::TaskSucceeded {
            task_key: TaskKey::new("all"),
            attempt: None,
            execution_id: None,
        },
    };
    let re = translate::from_engine(EventSource::Snakemake, &w);
    assert_eq!(re.kind(), Kind::TaskSucceeded);
    assert_eq!(re.run_id.unwrap().as_str(), "r1");
    assert_eq!(re.task_key.unwrap().as_str(), "all");
    assert!(re.batch_job_id.is_none());
    assert!(re.attempt.is_none());

    let f = EngineEvent {
        schema_version: 1,
        ts: ts(),
        run_id: RunId::new("r1"),
        body: EngineBody::TaskFailed {
            task_key: TaskKey::new("all"),
            attempt: None,
            execution_id: None,
        },
    };
    let rf = translate::from_engine(EventSource::Snakemake, &f);
    assert_eq!(rf.kind(), Kind::TaskFailed);
    assert!(rf.batch_job_id.is_none());
    // Without an attempt the key has no attempt slot.
    assert_eq!(rf.dedup_key, "snakemake:r1:all::task_failed");
}

#[test]
fn a_retried_local_rule_s_terminals_are_distinct_facts() {
    // A local rule the engine retries fails once per attempt; each failure carries its attempt, so
    // the second is not dropped as a redelivery of the first.
    let failed = |attempt: Option<u32>| {
        translate::from_engine(
            EventSource::Snakemake,
            &EngineEvent {
                schema_version: 1,
                ts: ts(),
                run_id: RunId::new("r1"),
                body: EngineBody::TaskFailed {
                    task_key: TaskKey::new("all"),
                    attempt: attempt.map(Attempt),
                    execution_id: None,
                },
            },
        )
    };
    let (first, second) = (failed(Some(1)), failed(Some(2)));
    assert_eq!(first.attempt, Some(Attempt(1)));
    assert_eq!(first.dedup_key, "snakemake:r1:all:1:task_failed");
    assert_ne!(first.dedup_key, second.dedup_key);
    assert_eq!(first.dedup_key, failed(Some(1)).dedup_key, "a redelivery dedupes");
    // The attempt is read from the wire when present and optional when not.
    for (kind, attempt) in
        [("task_succeeded", Some(3)), ("task_cached", Some(1)), ("task_failed", None)]
    {
        let mut wire =
            serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": kind, "task_key": "t"});
        if let Some(a) = attempt {
            wire["attempt"] = a.into();
        }
        let ev: EngineEvent = serde_json::from_value(wire).unwrap();
        let re = translate::from_engine(EventSource::Snakemake, &ev);
        assert_eq!(re.attempt, attempt.map(Attempt), "{kind}");
    }
}

#[test]
fn a_local_terminal_re_run_at_the_same_attempt_is_a_distinct_fact() {
    // Nextflow's executor retry re-runs a task without advancing its attempt; each execution has
    // its own trace task_id, which the plugin sends as `execution_id`. Two failures at one attempt
    // are then two facts, while a redelivery of one (same execution) still dedupes.
    let terminal = |kind: &str, execution: Option<&str>| {
        let mut wire = serde_json::json!({
            "ts": 1.0, "run_id": "r1", "kind": kind, "task_key": "FOO (1)", "attempt": 1,
        });
        if let Some(id) = execution {
            wire["execution_id"] = id.into();
        }
        let ev: EngineEvent = serde_json::from_value(wire).unwrap();
        translate::from_engine(EventSource::Nextflow, &ev)
    };
    for kind in ["task_failed", "task_succeeded", "task_cached"] {
        let (first, second) = (terminal(kind, Some("11")), terminal(kind, Some("12")));
        assert_eq!(first.dedup_key, format!("nextflow:r1:FOO (1):1:{kind}:11"));
        assert_ne!(first.dedup_key, second.dedup_key, "{kind}");
        assert_eq!(first.dedup_key, terminal(kind, Some("11")).dedup_key, "a redelivery dedupes");
        assert_eq!(first.attempt, Some(Attempt(1)));
        // Without it the key has no discriminator.
        assert_eq!(terminal(kind, None).dedup_key, format!("nextflow:r1:FOO (1):1:{kind}"));
    }
}

#[test]
fn from_engine_stamps_the_supplied_source_and_dedup_key() {
    // The engine wire shape is engine-agnostic; the same event folds under whichever engine source
    // the ingest handler resolves. A Nextflow run's event must record EventSource::Nextflow (and a
    // nextflow-prefixed dedup key), not the Snakemake default.
    let ev = EngineEvent {
        schema_version: 1,
        ts: ts(),
        run_id: RunId::new("r1"),
        body: EngineBody::RunStarted { session_id: None },
    };
    let nf = translate::from_engine(EventSource::Nextflow, &ev);
    assert_eq!(nf.source, EventSource::Nextflow);
    assert_eq!(nf.dedup_key, "nextflow:r1:::run_started");

    let sm = translate::from_engine(EventSource::Snakemake, &ev);
    assert_eq!(sm.source, EventSource::Snakemake);
    assert_eq!(sm.dedup_key, "snakemake:r1:::run_started");
}

#[test]
fn run_started_carries_the_engine_session_id() {
    let ev: EngineEvent = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "ts": 1.0, "run_id": "r1",
        "kind": "run_started", "session_id": "3f6c2a1e-0000-4000-8000-000000000000"
    }))
    .unwrap();
    let re = translate::from_engine(EventSource::Nextflow, &ev);
    assert_eq!(
        re.data,
        EventData::RunStarted { session_id: Some("3f6c2a1e-0000-4000-8000-000000000000".into()) }
    );
}

#[test]
fn run_started_without_session_id_is_none() {
    // The Snakemake logger (and nf-biffix before this field) sends a bare run_started.
    let ev: EngineEvent = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "ts": 1.0, "run_id": "r1", "kind": "run_started"
    }))
    .unwrap();
    let re = translate::from_engine(EventSource::Snakemake, &ev);
    assert_eq!(re.data, EventData::RunStarted { session_id: None });
}

#[test]
fn task_created_carries_its_deps_and_defaults_to_none() {
    let with: EngineEvent = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "ts": 1.0, "run_id": "r1",
        "kind": "task_created", "task_key": "merge", "group": "merge",
        "deps": ["align:A", "align:B"]
    }))
    .unwrap();
    assert_eq!(
        translate::from_engine(EventSource::Snakemake, &with).data,
        EventData::TaskCreated {
            group: Some("merge".into()),
            deps: vec!["align:A".into(), "align:B".into()],
        }
    );
    let without: EngineEvent = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "ts": 1.0, "run_id": "r1", "kind": "task_created", "task_key": "a"
    }))
    .unwrap();
    assert_eq!(
        translate::from_engine(EventSource::Snakemake, &without).data,
        EventData::TaskCreated { group: None, deps: vec![] }
    );
}

// ── status → kind tables (every status, both roles) ──

fn batch_event(status: BatchStatus, is_coordinator: bool) -> AwsBatchEvent {
    AwsBatchEvent {
        ts: ts(),
        batch_job_id: BatchJobId::new("job-1"),
        status,
        exit_code: None,
        status_reason: None,
        log_stream: Some("job/stream".into()),
        instance_id: None,
        queue: None,
        is_coordinator,
        prior_attempts: None,
        requested_vcpus: None,
        requested_memory_mib: None,
    }
}

#[test]
fn every_batch_status_maps_to_its_worker_kind() {
    let stream = Some("job/stream".to_string());
    let started = EventData::TaskStarted { started_at: Some(ts()), log_stream: stream.clone() };
    for (status, expected) in [
        (BatchStatus::Submitted, EventData::TaskQueued {}),
        (BatchStatus::Pending, EventData::TaskQueued {}),
        (BatchStatus::Runnable, EventData::TaskQueued {}),
        (BatchStatus::Starting, started.clone()),
        (BatchStatus::Running, started),
        // No exit code in the event ⇒ 0 on success, -1 on failure; the terminal keeps the stream.
        (
            BatchStatus::Succeeded,
            EventData::TaskSucceeded { exit_code: 0, log_stream: stream.clone() },
        ),
        (
            BatchStatus::Failed,
            EventData::TaskFailed {
                exit_code: -1,
                termination_category: None,
                status_reason: None,
                log_stream: stream.clone(),
            },
        ),
    ] {
        let re = translate::from_aws_batch(&batch_event(status.clone(), false));
        assert_eq!(re.data, expected, "{status:?}");
        assert_eq!(re.dedup_key, format!("aws-batch:job-1:{}", status.as_wire()));
    }
}

#[test]
fn every_batch_status_maps_to_its_coordinator_kind() {
    let stream = Some("job/stream".to_string());
    for (status, expected) in [
        (BatchStatus::Submitted, EventData::CoordinatorSubmitted {}),
        (BatchStatus::Pending, EventData::CoordinatorSubmitted {}),
        (BatchStatus::Runnable, EventData::CoordinatorSubmitted {}),
        (BatchStatus::Starting, EventData::CoordinatorRunning { log_stream: stream.clone() }),
        (BatchStatus::Running, EventData::CoordinatorRunning { log_stream: stream.clone() }),
        (BatchStatus::Succeeded, EventData::CoordinatorSucceeded { log_stream: stream.clone() }),
        (
            BatchStatus::Failed,
            EventData::CoordinatorFailed {
                log_stream: stream.clone(),
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ] {
        let re = translate::from_aws_batch(&batch_event(status.clone(), true));
        assert_eq!(re.data, expected, "{status:?}");
    }
}

#[test]
fn a_coordinator_failure_keeps_batch_s_account_of_the_stop() {
    // A coordinator that never started: no log, so Batch's reason is the only record of why.
    let mut ev = batch_event(BatchStatus::Failed, true);
    ev.status_reason = translate::batch_reason(
        Some("Task failed to start"),
        Some("CannotPullContainerError: ref/runtime:sm-9 not found"),
    );
    assert_eq!(
        translate::from_aws_batch(&ev).data,
        EventData::CoordinatorFailed {
            log_stream: Some("job/stream".into()),
            exit_code: None,
            status_reason: Some(
                "Task failed to start: CannotPullContainerError: ref/runtime:sm-9 not found".into()
            ),
            termination_category: None,
        }
    );
    // One that ran and was killed: its exit code and the category read from the reason.
    let mut ev = batch_event(BatchStatus::Failed, true);
    ev.exit_code = Some(137);
    ev.status_reason = Some("OutOfMemoryError: Container killed due to memory usage".into());
    match translate::from_aws_batch(&ev).data {
        EventData::CoordinatorFailed { exit_code, status_reason, termination_category, .. } => {
            assert_eq!(exit_code, Some(137));
            assert_eq!(status_reason.as_deref(), ev.status_reason.as_deref());
            assert_eq!(termination_category, Some(TerminationCategory::Oom));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_worker_terminal_carries_its_exit_code_and_log_stream() {
    // An attempt seen only through its terminal (the reconcile path) still gets its log.
    let mut ev = batch_event(BatchStatus::Failed, false);
    ev.exit_code = Some(137);
    ev.log_stream = Some("final/stream".into());
    match translate::from_aws_batch(&ev).data {
        EventData::TaskFailed { exit_code, log_stream, .. } => {
            assert_eq!(exit_code, 137);
            assert_eq!(log_stream.as_deref(), Some("final/stream"));
        }
        other => panic!("expected TaskFailed, got {other:?}"),
    }
}

#[test]
fn every_codebuild_status_maps_to_its_build_kind() {
    use biffix_core::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};
    let build = |status: CodeBuildStatus, reason: Option<&str>| CodeBuildEvent {
        ts: ts(),
        build_arn: "arn:build/1".into(),
        project: "p".into(),
        status,
        subject: "runtime:k".into(),
        outputs: [("DIGEST".to_string(), "sha256:x".to_string())].into(),
        reason: reason.map(String::from),
        logs_url: Some("https://logs".into()),
    };
    let (subject, build_arn, logs_url) =
        ("runtime:k".to_string(), "arn:build/1".to_string(), Some("https://logs".to_string()));
    assert_eq!(
        translate::from_aws_codebuild(&build(CodeBuildStatus::InProgress, None)).data,
        EventData::BuildStarted {
            subject: subject.clone(),
            build_arn: build_arn.clone(),
            logs_url: logs_url.clone(),
        }
    );
    assert_eq!(
        translate::from_aws_codebuild(&build(CodeBuildStatus::Succeeded, None)).data,
        EventData::BuildSucceeded {
            subject: subject.clone(),
            build_arn: build_arn.clone(),
            outputs: [("DIGEST".to_string(), "sha256:x".to_string())].into(),
            logs_url: logs_url.clone(),
        }
    );
    for (status, wire) in [
        (CodeBuildStatus::Failed, "FAILED"),
        (CodeBuildStatus::Fault, "FAULT"),
        (CodeBuildStatus::Stopped, "STOPPED"),
        (CodeBuildStatus::TimedOut, "TIMED_OUT"),
    ] {
        for (reason, expected) in
            [(None, wire.to_string()), (Some("BUILD: exit 2"), format!("{wire}: BUILD: exit 2"))]
        {
            let re = translate::from_aws_codebuild(&build(status.clone(), reason));
            assert_eq!(
                re.data,
                EventData::BuildFailed {
                    subject: subject.clone(),
                    build_arn: build_arn.clone(),
                    reason: expected,
                    logs_url: logs_url.clone(),
                },
                "{status:?} {reason:?}"
            );
            assert_eq!(re.dedup_key, format!("aws-codebuild:arn:build/1:{wire}"));
        }
    }
}

// ── unknown wire values: stored under a kind, raw event nested, envelope intact ──

/// Serialize and re-read a RunEvent both ways the log does (text and `Value`), asserting it comes
/// back unchanged with the RunEvent contract's own schema version.
fn assert_round_trips(re: &biffix_core::event::RunEvent) {
    assert_eq!(re.schema_version, biffix_core::event::SCHEMA_VERSION);
    let text = serde_json::to_string(re).unwrap();
    let back: biffix_core::event::RunEvent = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, re, "text round-trip (no duplicate keys)");
    let value = serde_json::to_value(re).unwrap();
    assert_eq!(value["schema_version"], biffix_core::event::SCHEMA_VERSION);
    let back: biffix_core::event::RunEvent = serde_json::from_value(value).unwrap();
    assert_eq!(&back, re, "value round-trip");
}

#[test]
fn an_unknown_batch_status_is_stored_under_a_kind_with_the_raw_event_nested() {
    let mut ev = batch_event(BatchStatus::Unknown("SOME_NEW_STATE".into()), false);
    ev.queue = Some("q".into());
    let re = translate::from_aws_batch(&ev);
    assert_eq!(re.kind(), Kind::Unknown("aws_batch_some_new_state".into()));
    assert!(!re.data.is_degraded_known_kind());
    let EventData::Unknown(payload) = &re.data else {
        panic!("expected Unknown, got {:?}", re.data);
    };
    assert_eq!(payload["raw"], serde_json::to_value(&ev).unwrap());
    assert_round_trips(&re);
    let coordinator = translate::from_aws_batch(&batch_event(
        BatchStatus::Unknown("SOME_NEW_STATE".into()),
        true,
    ));
    assert_eq!(coordinator.kind().as_str(), "aws_batch_some_new_state");
    assert_round_trips(&coordinator);
}

#[test]
fn an_unknown_codebuild_status_and_ec2_kind_are_stored_under_a_kind() {
    use biffix_core::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};
    let build = CodeBuildEvent {
        ts: ts(),
        build_arn: "arn:build/1".into(),
        project: "p".into(),
        status: CodeBuildStatus::Unknown("QUEUED".into()),
        subject: "runtime:k".into(),
        outputs: Default::default(),
        reason: None,
        logs_url: None,
    };
    let re = translate::from_aws_codebuild(&build);
    assert_eq!(re.kind().as_str(), "aws_codebuild_queued");
    assert_round_trips(&re);

    let ec2 = Ec2Event {
        ts: ts(),
        instance_id: InstanceId::new("i-1"),
        kind: Ec2EventKind::Unknown("Rebalance_Recommendation".into()),
    };
    let re = translate::from_aws_ec2(&ec2);
    assert_eq!(re.kind().as_str(), "aws_ec2_rebalance_recommendation");
    assert_round_trips(&re);
    // Keyed on its kind: it never collapses onto a spot warning at the same instance and time.
    let spot = translate::from_aws_ec2(&Ec2Event {
        kind: Ec2EventKind::SpotInterruptionWarning,
        ..ec2.clone()
    });
    assert_ne!(re.dedup_key, spot.dedup_key);
    assert_eq!(re.dedup_key, format!("aws-ec2:i-1:rebalance_recommendation:{}", ts().as_micros()));
}

// ── engine elements: unknown kinds are stored, known ones parsed strictly ──

#[test]
fn an_unknown_engine_kind_is_stored_not_rejected() {
    let element = serde_json::json!({
        "schema_version": 2, "ts": 5.0, "run_id": "r1",
        "kind": "engine_task_memory_sample", "task_key": "align:A", "rss": 123
    });
    let re = translate::from_engine_value(EventSource::Nextflow, element.clone()).unwrap();
    assert_eq!(re.kind(), Kind::Unknown("engine_task_memory_sample".into()));
    assert_eq!(re.run_id, Some(RunId::new("r1")));
    assert_eq!(re.ts, Timestamp::from_epoch_seconds(5.0));
    assert_eq!(re.source, EventSource::Nextflow);
    assert!(re.task_key.is_none(), "an unknown kind is not interpreted");
    let EventData::Unknown(payload) = &re.data else {
        panic!("expected Unknown");
    };
    assert_eq!(payload["raw"], element);
    assert!(re.dedup_key.starts_with("nextflow:r1:::engine_task_memory_sample:"));
    assert_round_trips(&re);

    // A redelivery (keys in another order) dedupes; a different fact of the same kind does not.
    let reordered: serde_json::Value = serde_json::from_str(
        r#"{"rss": 123, "task_key": "align:A", "kind": "engine_task_memory_sample",
            "run_id": "r1", "ts": 5.0, "schema_version": 2}"#,
    )
    .unwrap();
    let again = translate::from_engine_value(EventSource::Nextflow, reordered).unwrap();
    assert_eq!(again.dedup_key, re.dedup_key);
    let mut other = element;
    other["rss"] = serde_json::json!(456);
    let other = translate::from_engine_value(EventSource::Nextflow, other).unwrap();
    assert_ne!(other.dedup_key, re.dedup_key);
}

#[test]
fn a_known_engine_kind_is_parsed_strictly() {
    use translate::EngineEventError;
    let ok = serde_json::json!({"schema_version": 1, "ts": 1.0, "run_id": "r1",
        "kind": "run_progress", "completed": 1, "total": 2});
    let re = translate::from_engine_value(EventSource::Snakemake, ok.clone()).unwrap();
    let parsed: EngineEvent = serde_json::from_value(ok).unwrap();
    assert_eq!(re, translate::from_engine(EventSource::Snakemake, &parsed));

    let malformed = serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": "run_progress",
        "completed": 1});
    assert!(matches!(
        translate::from_engine_value(EventSource::Snakemake, malformed),
        Err(EngineEventError::Malformed { .. })
    ));
    // A RunEvent kind no engine may produce is refused, not stored under its name — and so is an
    // unknown kind outside the engine namespace, which a later build could add as a control-plane
    // fact and would then fold from the engine's row.
    for kind in
        ["run_submitted", "coordinator_failed", "task_started", "run_approved", "cost_override"]
    {
        let forged = serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": kind});
        assert!(
            matches!(
                translate::from_engine_value(EventSource::Snakemake, forged),
                Err(EngineEventError::NotAnEngineKind(_))
            ),
            "{kind}"
        );
    }
    // Each rejection rule, told apart by its error. The tag check runs before the namespace check,
    // so a malformed tag is InvalidKind with or without the `engine_` prefix, and the envelope is
    // parsed only for a well-formed `engine_*` kind.
    type Expect = fn(&EngineEventError) -> bool;
    let too_long = format!("engine_{}", "x".repeat(58));
    assert_eq!(too_long.len(), 65);
    let cases: Vec<(serde_json::Value, &str, Expect)> = vec![
        (serde_json::json!({"ts": 1.0, "run_id": "r1"}), "MissingKind", |e| {
            matches!(e, EngineEventError::MissingKind)
        }),
        (serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": 7}), "MissingKind", |e| {
            matches!(e, EngineEventError::MissingKind)
        }),
        (serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": "has:colon"}), "InvalidKind", |e| {
            matches!(e, EngineEventError::InvalidKind(_))
        }),
        (serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": "Upper"}), "InvalidKind", |e| {
            matches!(e, EngineEventError::InvalidKind(_))
        }),
        (
            serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": "engine_has:colon"}),
            "InvalidKind",
            |e| matches!(e, EngineEventError::InvalidKind(_)),
        ),
        (
            serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": "engine_Upper"}),
            "InvalidKind",
            |e| matches!(e, EngineEventError::InvalidKind(_)),
        ),
        (
            serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": too_long}),
            "InvalidKind",
            // A well-formed tag that is too long is told why.
            |e| matches!(e, EngineEventError::InvalidKind(_)) && e.to_string().contains("64"),
        ),
        // Outside the namespace: refused before the (here missing) envelope is looked at.
        (serde_json::json!({"run_id": "r1", "kind": "future_kind"}), "NotAnEngineKind", |e| {
            matches!(e, EngineEventError::NotAnEngineKind(_))
        }),
        // Inside it, the envelope every engine event carries is still required.
        (serde_json::json!({"run_id": "r1", "kind": "engine_future_kind"}), "Malformed", |e| {
            matches!(e, EngineEventError::Malformed { .. })
        }),
        (serde_json::json!({"ts": 1.0, "kind": "engine_future_kind"}), "Malformed", |e| {
            matches!(e, EngineEventError::Malformed { .. })
        }),
    ];
    for (bad, want, expect) in cases {
        match translate::from_engine_value(EventSource::Snakemake, bad.clone()) {
            Err(e) => assert!(expect(&e), "{bad}: want {want}, got {e:?}"),
            Ok(re) => panic!("{bad}: want {want}, got Ok({re:?})"),
        }
    }
    // The longest tag the rule admits (64 chars) is stored.
    let longest = format!("engine_{}", "x".repeat(57));
    assert!(translate::from_engine_value(
        EventSource::Snakemake,
        serde_json::json!({"ts": 1.0, "run_id": "r1", "kind": longest})
    )
    .is_ok());
}

/// The "unknown-field tolerant" half of the RunEvent contract: a known kind that carries a field a
/// newer producer added still parses, and to the same event as without it — for an engine element,
/// for a stored RunEvent's envelope, and for a stored anchor's payload.
#[test]
fn a_known_kind_tolerates_fields_a_newer_producer_adds() {
    let plain = serde_json::json!({"schema_version": 1, "ts": 1.0, "run_id": "r1",
        "kind": "run_progress", "completed": 1, "total": 2});
    let mut newer = plain.clone();
    newer["future_field"] = serde_json::json!({"x": 1});
    let want = translate::from_engine_value(EventSource::Snakemake, plain).unwrap();
    let got = translate::from_engine_value(EventSource::Snakemake, newer)
        .expect("an extra field on a known engine kind is tolerated");
    assert_eq!(got, want, "same data and dedup key");

    // A stored RunEvent with an unknown top-level envelope field.
    let mut stored = serde_json::to_value(&want).unwrap();
    stored["future_envelope_field"] = serde_json::json!(true);
    let back: biffix_core::event::RunEvent = serde_json::from_value(stored).unwrap();
    assert_eq!(back, want);

    // A stored anchor whose payload has a key this build does not know.
    let anchor = biffix_core::event::run_submitted(
        &RunId::new("r1"),
        ts(),
        biffix_core::event::RunSubmitted::new(
            "wf",
            "sha",
            "sha256:x",
            serde_json::json!({}),
            vec![],
        ),
    );
    let mut stored = serde_json::to_value(&anchor).unwrap();
    stored["future_payload_key"] = serde_json::json!("x");
    let back: biffix_core::event::RunEvent = serde_json::from_value(stored).unwrap();
    assert!(!back.data.is_degraded_known_kind());
    assert_eq!(back, anchor);
}

#[test]
fn no_non_engine_kind_is_in_the_engine_namespace() {
    // The engine namespace is what lets an engine store a kind this build does not know; a
    // control-plane or AWS kind under it could be planted by any capability-token holder.
    // A known kind may be in it only as an engine kind (a new engine kind is named under it).
    for kind in Kind::KNOWN {
        assert!(
            !kind.as_str().starts_with(translate::ENGINE_KIND_PREFIX)
                || EngineBody::KINDS.contains(&kind.as_str()),
            "{} is a known non-engine kind in the engine namespace",
            kind.as_str()
        );
    }
    // And every engine kind but those that predate the rule is under it.
    const PREDATING: [&str; 12] = [
        "run_started",
        "run_progress",
        "run_succeeded",
        "run_failed",
        "run_dag",
        "task_created",
        "task_submitted",
        "task_succeeded",
        "task_cached",
        "task_failed",
        "task_retrying",
        "task_outputs",
    ];
    for kind in EngineBody::KINDS {
        assert!(
            PREDATING.contains(kind) || kind.starts_with(translate::ENGINE_KIND_PREFIX),
            "{kind}"
        );
    }
}

#[test]
fn a_regrown_dag_is_a_distinct_fact() {
    use biffix_core::event::{Dag, DagNode};
    let dag_event = |ids: &[&str]| EngineEvent {
        schema_version: 1,
        ts: ts(),
        run_id: RunId::new("r1"),
        body: EngineBody::RunDag {
            dag: Dag {
                nodes: ids.iter().map(|id| DagNode { id: id.to_string() }).collect(),
                edges: vec![],
            },
        },
    };
    let key =
        |ids: &[&str]| translate::from_engine(EventSource::Snakemake, &dag_event(ids)).dedup_key;
    assert_eq!(key(&["a", "b"]), key(&["b", "a"]), "a redelivery dedupes");
    assert_ne!(key(&["a", "b"]), key(&["a", "b", "c"]), "a grown graph is kept");
}

#[test]
fn aws_batch_requested_resources_ride_the_event_and_are_omitted_when_unknown() {
    // Every status carries the job's request, so whichever Batch event arrives first fills it.
    for status in [BatchStatus::Runnable, BatchStatus::Running, BatchStatus::Succeeded] {
        let w = AwsBatchEvent {
            requested_vcpus: Some(4.0),
            requested_memory_mib: Some(30_000),
            ..batch_event(status, false)
        };
        let re = translate::from_aws_batch(&w);
        assert_eq!(re.requested_vcpus, Some(4.0));
        assert_eq!(re.requested_memory_mib, Some(30_000));
        let json = serde_json::to_value(&re).unwrap();
        assert_eq!(json["requested_vcpus"], serde_json::json!(4.0));
        assert_eq!(json["requested_memory_mib"], serde_json::json!(30_000));
    }
    // Unknown ⇒ absent from the wire (additive: an older consumer sees the old shape), and an event
    // from before the field reads back as unknown.
    let re = translate::from_aws_batch(&batch_event(BatchStatus::Running, false));
    let json = serde_json::to_value(&re).unwrap();
    assert!(json.get("requested_vcpus").is_none());
    assert!(json.get("requested_memory_mib").is_none());
    let back: biffix_core::event::RunEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back.requested_vcpus, None);
    assert_eq!(back.requested_memory_mib, None);
}
