//! Engine verdicts (`engine_task_verdict`).

use super::*;

#[test]
fn an_engine_failed_verdict_outranks_aws_success_for_the_task_in_any_order() {
    // Nextflow failed a task whose Batch job exited 0 (it did not write its outputs). The task is
    // failed; the attempt keeps what AWS said.
    let log = vec![
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        batch(
            3.0,
            "job-1",
            EventData::TaskStarted { started_at: None, log_stream: Some("job-1/live".into()) },
        ),
        aws_succeeded(4.0, "job-1"),
        verdict(4.5, "r1", "align", Some(1), TaskVerdict::Failed),
        coord(
            6.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let run = &p.runs["r1"];
    let task = &run.tasks["align"];
    assert_eq!(task.state(), TaskState::Failed);
    assert_eq!(run.effective_task_state(task), TaskState::Failed);
    assert!(!run.is_stranded(task), "the task's own failure, not the run's");
    let ap = &task.attempts["job-1"];
    assert_eq!(ap.state, AttemptState::Succeeded, "AWS's fact is kept");
    assert_eq!(task.counted_state(ap), AttemptState::Failed);
    assert_eq!(ap.exit_code, Some(0));
    assert_eq!(ap.log_stream.as_deref(), Some("job-1/final"));
    let v = task.verdict().expect("the verdict explains the failure");
    assert_eq!(v.category, Some(TerminationCategory::MissingOutput));
    assert_eq!(v.reason.as_deref(), Some("Missing output file(s) `out.txt`"));
    assert_eq!(run.task_counts().failed, 1);
    // A verdict that reaches the fold before AWS's terminal already ends the task.
    let early = Projection::replay(&log[..2].iter().chain(&log[3..4]).cloned().collect::<Vec<_>>());
    assert_eq!(task_state(&early, "r1", "align"), TaskState::Failed);
}

#[test]
fn a_retry_after_a_verdict_can_still_succeed_in_any_order() {
    let log = vec![
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        aws_succeeded(3.0, "job-1"),
        verdict(4.0, "r1", "align", Some(1), TaskVerdict::Failed),
        run_level(
            4.0,
            "r1",
            EventData::TaskRetrying { prev_attempt: Attempt(1), next_attempt: Attempt(2) },
        )
        .with_task("align"),
        task_submitted(5.0, "r1", "align", 2, "job-2"),
        aws_succeeded(6.0, "job-2"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let task = &p.runs["r1"].tasks["align"];
    assert_eq!(task.state(), TaskState::Succeeded);
    assert_eq!(task.display_attempt().unwrap().batch_job_id.as_str(), "job-2");
    assert_eq!(task.verdict(), None, "the display attempt was not rejected");
    // Between the verdict and the retry's submit, the task is between attempts, not failed.
    let gap = Projection::replay(&log[..4]);
    assert_eq!(task_state(&gap, "r1", "align"), TaskState::Running);
    // Without the retry, the rejected attempt is the display attempt and the task failed.
    let no_retry = Projection::replay(&log[..3]);
    assert_eq!(task_state(&no_retry, "r1", "align"), TaskState::Failed);
}

#[test]
fn a_verdict_does_not_reject_a_later_re_run_under_the_same_attempt_number() {
    // An engine may re-run a task without advancing its attempt number; a verdict on the first
    // execution must not also fail the re-run submitted after it.
    let log = vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        aws_succeeded(1.5, "job-1"),
        verdict(2.0, "r1", "align", Some(1), TaskVerdict::Failed),
        retrying(2.1, 2),
        task_submitted(3.0, "r1", "align", 1, "job-2"),
        aws_succeeded(4.0, "job-2"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let task = &p.runs["r1"].tasks["align"];
    assert_eq!(task.counted_state(&task.attempts["job-1"]), AttemptState::Failed);
    assert_eq!(task.counted_state(&task.attempts["job-2"]), AttemptState::Succeeded);
    assert_eq!(task.state(), TaskState::Succeeded);
}

#[test]
fn a_verdict_without_an_attempt_covers_attempts_submitted_by_then() {
    let log = vec![
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        aws_succeeded(3.0, "job-1"),
        verdict(4.0, "r1", "align", None, TaskVerdict::Failed),
        task_submitted(5.0, "r1", "align", 2, "job-2"),
        aws_succeeded(6.0, "job-2"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let task = &p.runs["r1"].tasks["align"];
    assert_eq!(task.counted_state(&task.attempts["job-1"]), AttemptState::Failed);
    assert_eq!(task.counted_state(&task.attempts["job-2"]), AttemptState::Succeeded);
    assert_eq!(task.state(), TaskState::Succeeded);
    assert_eq!(task_state(&Projection::replay(&log[..3]), "r1", "align"), TaskState::Failed);
}

#[test]
fn only_an_engine_s_known_failed_verdict_on_a_batch_attempt_folds() {
    let succeeded =
        || vec![task_submitted(2.0, "r1", "align", 1, "job-1"), aws_succeeded(3.0, "job-1")];
    // A verdict this build does not know.
    let mut log = succeeded();
    log.push(verdict(4.0, "r1", "align", Some(1), TaskVerdict::Unknown("warned".into())));
    assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Succeeded);
    // A `failed` verdict from anything but an engine.
    for source in [EventSource::AwsBatch, EventSource::BiffixControlPlane] {
        let mut forged = verdict(4.0, "r1", "align", Some(1), TaskVerdict::Failed);
        forged.source = source;
        assert!(!biffix_core::reducer::is_trusted_producer(&forged));
        let mut log = succeeded();
        log.push(forged);
        assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Succeeded);
    }
    // Another attempt's verdict.
    let mut log = succeeded();
    log.push(verdict(4.0, "r1", "align", Some(2), TaskVerdict::Failed));
    assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Succeeded);
    // A cached (attempt-less) task keeps its engine terminal.
    let cached = vec![
        engine_cached(2.0, "r1", "align"),
        verdict(3.0, "r1", "align", None, TaskVerdict::Failed),
    ];
    assert_incremental_equals_replay(&cached);
    let p = Projection::replay(&cached);
    assert_eq!(task_state(&p, "r1", "align"), TaskState::Succeeded);
    assert!(p.runs["r1"].tasks["align"].cached);
}

#[test]
fn two_verdicts_on_one_attempt_report_one_deterministically() {
    let mut other = verdict(4.0, "r1", "align", Some(1), TaskVerdict::Failed);
    if let EventData::EngineTaskVerdict { reason, .. } = &mut other.data {
        *reason = Some("A: exit status 3 is not valid".into());
    }
    other.dedup_key = "other".into();
    let log = vec![
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        aws_succeeded(3.0, "job-1"),
        verdict(4.0, "r1", "align", Some(1), TaskVerdict::Failed),
        other,
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(
        p.runs["r1"].tasks["align"].verdict().and_then(|v| v.reason.as_deref()),
        Some("A: exit status 3 is not valid")
    );
}
