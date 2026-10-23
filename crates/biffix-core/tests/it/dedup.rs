//! Dedup-key discipline: a *redelivery of the same fact* must yield the same key
//! (so the log's uniqueness constraint makes it a no-op), while *distinct facts* must yield
//! distinct keys (so repeatable kinds — progress, outputs, retries — are not silently dropped).

use biffix_core::dedup;
use biffix_core::ids::{BatchJobId, InstanceId, RunId, TaskKey};
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use biffix_core::translate;
use biffix_core::wire::aws_batch::{AwsBatchEvent, BatchStatus};
use biffix_core::wire::engine::{EngineBody, EngineEvent};

// ── pure key construction ──────────────────────────────────────────────────

#[test]
fn engine_key_is_deterministic_and_shape_stable() {
    let run = RunId::new("r1");
    let task = TaskKey::new("align:sample=A");
    let k1 = dedup::engine("snakemake", &run, Some(&task), Some(1), "task_submitted", None);
    let k2 = dedup::engine("snakemake", &run, Some(&task), Some(1), "task_submitted", None);
    assert_eq!(k1, k2, "same inputs must produce the same key");
    assert_eq!(k1, "snakemake:r1:align:sample=A:1:task_submitted");
}

#[test]
fn engine_discriminator_distinguishes_repeatable_facts() {
    let run = RunId::new("r1");
    let progress = |c: &str| dedup::engine("snakemake", &run, None, None, "run_progress", Some(c));
    assert_ne!(progress("2"), progress("5"), "distinct progress points must not collide");
    // Redelivery of the *same* progress fact still dedupes.
    assert_eq!(progress("5"), progress("5"));
}

#[test]
fn aws_batch_key_uses_wire_status_not_debug() {
    let key = dedup::aws_batch(&BatchJobId::new("job-1"), &BatchStatus::Failed, 0);
    assert_eq!(key, "aws-batch:job-1:FAILED", "the AWS wire form, not Rust Debug");
}

#[test]
fn a_batch_retry_s_edges_are_distinct_facts_and_first_attempt_keys_are_unchanged() {
    // Under a Batch retryStrategy the same job id goes RUNNABLE → STARTING → RUNNING again. Those
    // edges must not dedup onto the first attempt's (they would be silently dropped), while the
    // first attempt keeps the plain `(job, status)` key, so the live terminal and the reconciler's
    // (which may not know the count) are one fact.
    let job = BatchJobId::new("job-1");
    assert_eq!(dedup::aws_batch(&job, &BatchStatus::Running, 0), "aws-batch:job-1:RUNNING");
    assert_eq!(dedup::aws_batch(&job, &BatchStatus::Running, 1), "aws-batch:job-1:RUNNING:1");
    assert_ne!(
        dedup::aws_batch(&job, &BatchStatus::Running, 1),
        dedup::aws_batch(&job, &BatchStatus::Running, 2)
    );
    // A terminal is reached once per job however many attempts it took, so it keeps
    // `(job, status)` — the live terminal (attempts known) and the reconciled one (attempts
    // unknown) must still collapse to one.
    for status in [BatchStatus::Succeeded, BatchStatus::Failed] {
        assert_eq!(dedup::aws_batch(&job, &status, 2), dedup::aws_batch(&job, &status, 0));
    }

    let running = |prior_attempts: Option<u32>| AwsBatchEvent {
        ts: Timestamp::from_epoch_seconds(1.0),
        batch_job_id: job.clone(),
        status: BatchStatus::Running,
        exit_code: None,
        status_reason: None,
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: false,
        prior_attempts,
        requested_vcpus: None,
        requested_memory_mib: None,
    };
    let first = translate::from_aws_batch(&running(None)).dedup_key;
    assert_eq!(first, translate::from_aws_batch(&running(Some(0))).dedup_key);
    assert_ne!(first, translate::from_aws_batch(&running(Some(1))).dedup_key);
}

#[test]
fn ec2_interruption_key_shape() {
    let key = dedup::aws_ec2_interruption(&InstanceId::new("i-09"), 42);
    assert_eq!(key, "aws-ec2:i-09:interruption:42");
}

// ── translate-level: distinct facts get distinct keys (the silent-loss regression) ──

fn smk(ts: f64, body: EngineBody) -> EngineEvent {
    EngineEvent {
        schema_version: 1,
        ts: Timestamp::from_epoch_seconds(ts),
        run_id: RunId::new("r1"),
        body,
    }
}

#[test]
fn run_progress_events_do_not_collide() {
    let a = translate::from_engine(
        EventSource::Snakemake,
        &smk(2.0, EngineBody::RunProgress { completed: 2, total: 10 }),
    );
    let b = translate::from_engine(
        EventSource::Snakemake,
        &smk(3.0, EngineBody::RunProgress { completed: 5, total: 10 }),
    );
    assert_ne!(
        a.dedup_key, b.dedup_key,
        "successive run_progress must not dedup to one another (progress would freeze)"
    );
}

#[test]
fn run_progress_total_only_change_does_not_collide() {
    // Same `completed`, different `total` (a checkpoint grew the DAG) is a DISTINCT fact. Keying
    // the discriminator on `completed` alone dropped the update, freezing the run's progress
    // denominator.
    let a = translate::from_engine(
        EventSource::Snakemake,
        &smk(2.0, EngineBody::RunProgress { completed: 5, total: 10 }),
    );
    let b = translate::from_engine(
        EventSource::Snakemake,
        &smk(3.0, EngineBody::RunProgress { completed: 5, total: 20 }),
    );
    assert_ne!(a.dedup_key, b.dedup_key, "a total-only progress change must not dedup away");
    // Redelivery of the identical (completed, total) still dedupes.
    let c = translate::from_engine(
        EventSource::Snakemake,
        &smk(4.0, EngineBody::RunProgress { completed: 5, total: 20 }),
    );
    assert_eq!(b.dedup_key, c.dedup_key);
}

#[test]
fn task_submitted_distinct_batch_jobs_do_not_collide() {
    use biffix_core::ids::Attempt;
    let submit = |job: &str| EngineBody::TaskSubmitted {
        task_key: TaskKey::new("align:sample=A"),
        attempt: Attempt(1),
        batch_job_id: BatchJobId::new(job),
        image: None,
    };
    let a = translate::from_engine(EventSource::Snakemake, &smk(2.0, submit("job-aaa")));
    let b = translate::from_engine(EventSource::Snakemake, &smk(2.0, submit("job-bbb")));
    assert_ne!(
        a.dedup_key, b.dedup_key,
        "the same (run,task,attempt) submitted as two different Batch jobs are distinct facts"
    );
    // A genuine redelivery (same job id) still dedupes.
    let c = translate::from_engine(EventSource::Snakemake, &smk(3.0, submit("job-aaa")));
    assert_eq!(a.dedup_key, c.dedup_key);
}

#[test]
fn task_retrying_events_do_not_collide() {
    use biffix_core::ids::Attempt;
    let retry = |prev: u32, next: u32| EngineBody::TaskRetrying {
        task_key: TaskKey::new("t"),
        prev_attempt: Attempt(prev),
        next_attempt: Attempt(next),
    };
    let a = translate::from_engine(EventSource::Snakemake, &smk(2.0, retry(1, 2)));
    let b = translate::from_engine(EventSource::Snakemake, &smk(3.0, retry(2, 3)));
    assert_ne!(a.dedup_key, b.dedup_key, "a second retry of the same task must not dedup away");
    // A redelivery of the same retry (at another time) still dedupes.
    let again = translate::from_engine(EventSource::Snakemake, &smk(4.0, retry(1, 2)));
    assert_eq!(a.dedup_key, again.dedup_key);
}

#[test]
fn batch_terminal_key_is_independent_of_timestamp() {
    // The regression this guards: one attempt's terminal reported with two different times — the
    // live path falling back to the EventBridge envelope `time`, reconciliation using `stoppedAt` —
    // must still dedup to a single terminal. The key is keyed on (job, status), not the timestamp.
    let terminal = |ts: f64| AwsBatchEvent {
        ts: Timestamp::from_epoch_seconds(ts),
        batch_job_id: BatchJobId::new("job-1"),
        status: BatchStatus::Failed,
        exit_code: Some(137),
        status_reason: None,
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: false,
        prior_attempts: None,
        requested_vcpus: None,
        requested_memory_mib: None,
    };
    assert_eq!(
        translate::from_aws_batch(&terminal(100.0)).dedup_key,
        translate::from_aws_batch(&terminal(200.0)).dedup_key,
        "the same terminal timed two ways (envelope time vs stoppedAt) must collapse to one key"
    );
}

#[test]
fn task_outputs_dedup_is_order_independent_and_distinct_per_batch() {
    use biffix_core::event::{OutputRef, OutputRole};
    use biffix_core::ids::Attempt;
    let r = |uri: &str| OutputRef { s3_uri: uri.into(), path: None, etag: None, size: None };
    let batch = |attempt: u32, role: OutputRole, uris: &[&str]| EngineBody::TaskOutputs {
        task_key: Some(TaskKey::new("t")),
        attempt: Some(Attempt(attempt)),
        role: Some(role),
        reused: false,
        outputs: uris.iter().map(|u| r(u)).collect(),
    };
    let key = |body| translate::from_engine(EventSource::Snakemake, &smk(2.0, body)).dedup_key;
    // The same set in another order (or with a duplicate) is the same fact.
    assert_eq!(
        key(batch(1, OutputRole::Output, &["s3://b/a", "s3://b/c"])),
        key(batch(1, OutputRole::Output, &["s3://b/c", "s3://b/a", "s3://b/a"])),
    );
    // A different set, a different role, or another attempt is a distinct fact.
    let base = key(batch(1, OutputRole::Output, &["s3://b/a"]));
    assert_ne!(base, key(batch(1, OutputRole::Output, &["s3://b/b"])));
    assert_ne!(base, key(batch(1, OutputRole::Log, &["s3://b/a"])));
    assert_ne!(base, key(batch(2, OutputRole::Output, &["s3://b/a"])));
    let mut reused = batch(1, OutputRole::Output, &["s3://b/a"]);
    if let EngineBody::TaskOutputs { reused: r, .. } = &mut reused {
        *r = true;
    }
    assert_ne!(base, key(reused), "a reused batch is another fact");
    // A URI holding a line break cannot pass for two URIs.
    assert_ne!(
        key(batch(1, OutputRole::Output, &["s3://b/a\ns3://b/c"])),
        key(batch(1, OutputRole::Output, &["s3://b/a", "s3://b/c"])),
    );
}
