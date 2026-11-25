//! Reconciliation integration test against the real Postgres harness.
//!
//!   BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!     cargo test -p biffix-ingest --test it -- --ignored reconcile_pg::

use biffix_core::event::EventData;
use biffix_core::event::RunEvent;
use biffix_core::eventbridge::describe::{
    started_run_event, terminal_run_event, DescribedJobFacts, DescribedStartedJob,
    DescribedTerminalJob, TerminalStatus,
};
use biffix_core::ids::RunId;
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use biffix_ingest::store::reconcile::{
    live_children_of_run, run_id_for_coordinator, stale_attempts, stale_coordinators, LiveChild,
    StaleAttempt, StaleWindow,
};

use crate::common;
use biffix_core::testing::{self, RunEventExt};
use common::append;

fn event(job_id: &str, ts: f64, dedup_key: &str, source: EventSource, data: EventData) -> RunEvent {
    testing::event(ts, source, dedup_key, data)
        .with_run("run-reconcile")
        .with_task(&format!("align:sample={job_id}"))
        .with_attempt(1)
        .with_job(job_id)
}

/// Every job bound since the epoch is a candidate, stale at `cutoff_micros`.
fn window(cutoff_micros: i64, limit: i64, give_up_before_micros: i64) -> StaleWindow {
    StaleWindow { cutoff_micros, bound_since_micros: 0, give_up_before_micros, limit }
}

async fn append_submitted(pool: &sqlx::PgPool, job_id: &str, ts: f64) {
    append(
        pool,
        &event(
            job_id,
            ts,
            &format!("engine:{job_id}:submitted"),
            EventSource::Snakemake,
            EventData::TaskSubmitted { image: None },
        ),
    )
    .await
    .unwrap();
}

/// AWS Batch's SUCCEEDED for task attempt `job_id`.
async fn append_succeeded(pool: &sqlx::PgPool, job_id: &str, ts: f64) {
    append(
        pool,
        &event(
            job_id,
            ts,
            &format!("aws:{job_id}:succeeded"),
            EventSource::AwsBatch,
            EventData::TaskSucceeded { exit_code: 0, log_stream: None },
        ),
    )
    .await
    .unwrap();
}

/// The production `coordinator_submitted` binding of coordinator `job_id` to `run-<job_id>`, under
/// `dedup`.
async fn append_bound(pool: &sqlx::PgPool, job_id: &str, ts: f64, dedup: &str) {
    append(pool, &common::coordinator_bound(&format!("run-{job_id}"), ts, job_id, dedup))
        .await
        .unwrap();
}

/// The live children's job ids, in order.
fn child_ids(children: Vec<LiveChild>) -> Vec<String> {
    children.into_iter().map(|c| c.batch_job_id).collect()
}

/// The candidates' job ids, in order.
fn ids(candidates: Vec<StaleAttempt>) -> Vec<String> {
    candidates.into_iter().map(|c| c.batch_job_id).collect()
}

/// What DescribeJobs reports for task `job_id`, succeeded at 2 s with no stream, host or queue.
fn described(job_id: &str) -> DescribedTerminalJob {
    DescribedTerminalJob {
        batch_job_id: job_id.into(),
        status: TerminalStatus::Succeeded,
        stopped_at_millis: 2_000,
        exit_code: Some(0),
        status_reason: None,
        facts: DescribedJobFacts {
            log_stream: None,
            instance_id: None,
            queue: None,
            is_coordinator: false,
            requested: Default::default(),
        },
    }
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn stale_coordinators_selects_submitted_but_not_terminal() {
    let pool = common::isolated_pool("stale_coordinators_selects_submitted_but_not_terminal").await;

    // coord-stale: submitted only → a candidate. coord-done: submitted + succeeded → not.
    append_bound(&pool, "coord-stale", 1.0, "cp:coord-stale:sub").await;
    append_bound(&pool, "coord-done", 1.0, "cp:coord-done:sub").await;
    let done = testing::event(
        1.5,
        EventSource::AwsBatch,
        "aws:coord-done:succeeded",
        EventData::CoordinatorSucceeded { log_stream: None },
    )
    .with_run("run-coord-done")
    .with_job("coord-done");
    append(&pool, &done).await.unwrap();

    let candidates = stale_coordinators(&pool, window(2_000_000, 100, 0)).await.unwrap();
    let ids: Vec<&str> = candidates.iter().map(|c| c.batch_job_id.as_str()).collect();
    assert_eq!(ids, ["coord-stale"], "only the un-terminated coordinator is a candidate");

    // A task_submitted attempt is NOT a coordinator candidate (the two queries are disjoint).
    append_submitted(&pool, "some-task", 1.0).await;
    let after = stale_coordinators(&pool, window(2_000_000, 100, 0)).await.unwrap();
    let ids: Vec<&str> = after.iter().map(|c| c.batch_job_id.as_str()).collect();
    assert_eq!(ids, ["coord-stale"], "a task attempt must not appear as a coordinator candidate");
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn stale_attempt_is_described_then_idempotently_folded_through_append_path() {
    let pool = common::isolated_pool(
        "stale_attempt_is_described_then_idempotently_folded_through_append_path",
    )
    .await;

    append_submitted(&pool, "job-stale", 1.0).await;
    // "fresh" = submitted strictly AFTER both reconcile cutoffs used below (2.0s and 3.0s), so it
    // is never stale. At exactly 3.0s it would tie the second cutoff (the `<= cutoff` predicate is
    // inclusive) and wrongly count as stale, so pin it clearly later.
    append_submitted(&pool, "job-fresh", 4.0).await;
    append_submitted(&pool, "job-aws-terminal", 1.0).await;
    append_submitted(&pool, "job-engine-terminal", 1.0).await;
    append_succeeded(&pool, "job-aws-terminal", 1.5).await;
    append(
        &pool,
        &event(
            "job-engine-terminal",
            1.5,
            "engine:job-engine-terminal:succeeded",
            EventSource::Snakemake,
            EventData::TaskSucceeded { exit_code: 0, log_stream: None },
        ),
    )
    .await
    .unwrap();

    let candidates = stale_attempts(&pool, window(2_000_000, 100, 0)).await.unwrap();
    let candidate_ids =
        candidates.iter().map(|candidate| candidate.batch_job_id.as_str()).collect::<Vec<_>>();
    assert_eq!(candidate_ids, ["job-engine-terminal", "job-stale"]);
    assert_eq!(candidates[0].last_observed_micros, 1_000_000);

    // This is the typed fact the production adapter constructs from the real DescribeJobs result.
    let terminal = terminal_run_event(&DescribedTerminalJob {
        facts: DescribedJobFacts {
            log_stream: Some("batch/job-stale".into()),
            ..Default::default()
        },
        ..described("job-stale")
    });
    assert!(append(&pool, &terminal).await.unwrap());
    assert!(!append(&pool, &terminal).await.unwrap());

    let remaining = stale_attempts(&pool, window(3_000_000, 100, 0)).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].batch_job_id, "job-engine-terminal");
    let state: String = sqlx::query_scalar(
        "SELECT state FROM task_projection \
         WHERE run_id = 'run-reconcile' AND task_key = 'align:sample=job-stale'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "succeeded");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) AS count FROM run_events WHERE batch_job_id = 'job-stale'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2, "one submitted fact plus one deduped terminal fact");
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn reap_queries_map_coordinator_to_run_and_find_live_children() {
    let pool =
        common::isolated_pool("reap_queries_map_coordinator_to_run_and_find_live_children").await;

    // coord-1 is bound to run-coord-1 via its coordinator_submitted event.
    append_bound(&pool, "coord-1", 1.0, "cp:coord-1:sub").await;
    assert_eq!(
        run_id_for_coordinator(&pool, "coord-1").await.unwrap().as_deref(),
        Some("run-coord-1"),
    );
    assert_eq!(run_id_for_coordinator(&pool, "unknown").await.unwrap(), None);

    // run-reconcile has two submitted children; job-1 later reaches an aws-batch terminal.
    append_submitted(&pool, "job-1", 1.0).await;
    append_submitted(&pool, "job-2", 1.0).await;
    let live = child_ids(live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap());
    assert_eq!(live, ["job-1", "job-2"], "both children live before any terminal");
    // Keyset pagination: passing the first child as the cursor skips it and returns the rest, so
    // the reaper can page a run with more live children than one `limit`-sized page.
    // Asked while job-1 is still live, so an ignored cursor would return it.
    let paged =
        child_ids(live_children_of_run(&pool, "run-reconcile", Some("job-1"), 100).await.unwrap());
    assert_eq!(paged, ["job-2"], "keyset cursor skips ids <= the cursor");

    append_succeeded(&pool, "job-1", 2.0).await;
    let after = child_ids(live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap());
    assert_eq!(after, ["job-2"], "a terminated child is no longer live (reap skips it)");
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn live_children_of_run_pages_by_keyset_cursor_and_limit() {
    let pool = common::isolated_pool("live_children_of_run_pages_by_keyset_cursor_and_limit").await;

    // Three live (never terminal) children of one run, so only the cursor and the limit decide
    // which come back; the terminal filter cannot mask a cursor that is ignored.
    for job in ["child-1", "child-2", "child-3"] {
        let submitted = event(
            job,
            1.0,
            &format!("engine:{job}:submitted"),
            EventSource::Snakemake,
            EventData::TaskSubmitted { image: None },
        )
        .with_run("run-paged");
        append(&pool, &submitted).await.unwrap();
    }
    let first = child_ids(live_children_of_run(&pool, "run-paged", None, 2).await.unwrap());
    assert_eq!(first, ["child-1", "child-2"], "a full first page");
    let second =
        child_ids(live_children_of_run(&pool, "run-paged", Some("child-2"), 2).await.unwrap());
    assert_eq!(second, ["child-3"], "the cursor resumes after the last id");
    let done =
        child_ids(live_children_of_run(&pool, "run-paged", Some("child-3"), 2).await.unwrap());
    assert!(done.is_empty(), "then the pages end: {done:?}");
}

/// The stale backstop's bounded page rotates (least recently described first), so candidates that
/// never resolve cannot hold the head of the queue; a job Batch has omitted past the give-up
/// horizon drops out, and an omission streak survives repeat misses and a failed describe.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn stale_candidates_rotate_and_give_up_on_jobs_batch_forgot() {
    use biffix_ingest::store::reconcile::{record_describe_results, touch_described};
    let pool = common::isolated_pool("stale_candidates_rotate_and_give_up").await;
    append_submitted(&pool, "job-a", 1.0).await;
    append_submitted(&pool, "job-b", 2.0).await;
    append_submitted(&pool, "job-c", 3.0).await;
    let s = |v: &[&str]| v.iter().map(std::string::ToString::to_string).collect::<Vec<_>>();
    // Never described: oldest observation first.
    assert_eq!(
        ids(stale_attempts(&pool, window(10_000_000, 2, 0)).await.unwrap()),
        ["job-a", "job-b"]
    );

    // job-a and job-b described (still running): job-c, never described, now leads.
    let missing =
        record_describe_results(&pool, &s(&["job-a", "job-b"]), &[], 100, 0).await.unwrap();
    assert!(missing.is_empty());
    assert_eq!(
        ids(stale_attempts(&pool, window(10_000_000, 2, 0)).await.unwrap()),
        ["job-c", "job-a"]
    );

    // job-c omitted at 200, and again at 300: its streak keeps its start.
    let since = record_describe_results(&pool, &[], &s(&["job-c"]), 200, 0).await.unwrap();
    assert_eq!(since.get("job-c"), Some(&200));
    let since = record_describe_results(&pool, &[], &s(&["job-c"]), 300, 0).await.unwrap();
    assert_eq!(since.get("job-c"), Some(&200));
    // A failed describe rotates it without clearing the streak.
    touch_described(&pool, &s(&["job-c"]), 400).await.unwrap();
    assert_eq!(
        ids(stale_attempts(&pool, window(10_000_000, 10, 199)).await.unwrap()),
        ["job-a", "job-b", "job-c"],
        "omitted since 200, give-up horizon 199: still asked, rotated last"
    );
    assert_eq!(
        ids(stale_attempts(&pool, window(10_000_000, 10, 200)).await.unwrap()),
        ["job-a", "job-b"],
        "omitted since at or before the horizon: no longer a candidate"
    );

    // Described again: the streak ends and it is a candidate again.
    record_describe_results(&pool, &s(&["job-c"]), &[], 500, 0).await.unwrap();
    assert_eq!(
        ids(stale_attempts(&pool, window(10_000_000, 10, 1_000)).await.unwrap()),
        ["job-a", "job-b", "job-c"]
    );

    // Old rows of jobs Batch still knew are forgotten; an omitted job's row is kept.
    record_describe_results(&pool, &[], &s(&["job-gone"]), 50, 0).await.unwrap();
    record_describe_results(&pool, &[], &[], 600, 450).await.unwrap();
    let kept: Vec<String> =
        sqlx::query_scalar("SELECT batch_job_id FROM reconcile_describe ORDER BY batch_job_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(kept, ["job-c", "job-gone"]);
}

/// The translator shapes a job's Batch events by its queue (`BIFFIX_COORDINATOR_QUEUES`), while the
/// fold reads them by the job's binding, so a coordinator on an unlisted queue ends task-shaped and
/// a task on a listed one ends coordinator-shaped. Either is the job's terminal: the backstop and
/// the reaper must not keep describing (or terminating) a job the fold already ended.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_batch_terminal_of_either_shape_ends_the_job() {
    use biffix_ingest::store::reconcile::ended_runs_with_live_children;
    let pool = common::isolated_pool("a_batch_terminal_of_either_shape_ends_the_job").await;
    // As the translator emits a Batch fact: no run, only the job.
    let batch = |job: &str, ts: f64, data: EventData| {
        testing::event(ts, EventSource::AwsBatch, format!("aws:{job}:{ts}"), data).with_job(job)
    };
    let failed = || EventData::CoordinatorFailed {
        exit_code: Some(1),
        termination_category: None,
        status_reason: None,
        log_stream: None,
    };
    // A coordinator whose SUCCEEDED arrived task-shaped.
    append_bound(&pool, "coord-1", 1.0, "cp:coord-1:sub").await;
    append(
        &pool,
        &batch("coord-1", 1.5, EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    )
    .await
    .unwrap();
    // A task of an ended run whose FAILED arrived coordinator-shaped.
    append(&pool, &common::coordinator_bound("run-reconcile", 1.0, "coord-2", "cp:coord-2:sub"))
        .await
        .unwrap();
    append_submitted(&pool, "job-1", 1.0).await;
    append(&pool, &batch("job-1", 1.5, failed())).await.unwrap();
    append(&pool, &batch("coord-2", 1.6, failed())).await.unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM attempt_projection WHERE batch_job_id = 'job-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "failed", "the fold ended the task");

    assert!(stale_coordinators(&pool, window(2_000_000, 100, 0)).await.unwrap().is_empty());
    assert!(stale_attempts(&pool, window(2_000_000, 100, 0)).await.unwrap().is_empty());
    assert!(live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap().is_empty());
    assert!(ended_runs_with_live_children(&pool, 0, 100).await.unwrap().is_empty());
}

/// The backstop starts from the jobs bound in its window, so a poll's cost follows that window
/// rather than the log's whole history: a job bound before it is not a candidate, and neither is
/// one in it that ended or that Batch has forgotten.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn stale_candidates_are_the_jobs_bound_in_the_window() {
    use biffix_ingest::store::reconcile::record_describe_results;
    let pool = common::isolated_pool("stale_candidates_are_the_jobs_bound_in_the_window").await;
    append_submitted(&pool, "job-old", 1.0).await;
    append_submitted(&pool, "job-new", 5.0).await;
    append_submitted(&pool, "job-forgotten", 5.0).await;
    for job in ["job-ended-1", "job-ended-2"] {
        append_submitted(&pool, job, 5.0).await;
        append_succeeded(&pool, job, 6.0).await;
    }
    for (coord, ts) in [("coord-old", 1.0), ("coord-new", 5.0)] {
        append_bound(&pool, coord, ts, &format!("cp:{coord}")).await;
    }
    record_describe_results(&pool, &[], &["job-forgotten".into()], 7, 0).await.unwrap();
    let window = StaleWindow {
        cutoff_micros: 10_000_000,
        bound_since_micros: 2_000_000,
        give_up_before_micros: 7,
        limit: 100,
    };
    assert_eq!(ids(stale_attempts(&pool, window).await.unwrap()), ["job-new"]);
    assert_eq!(ids(stale_coordinators(&pool, window).await.unwrap()), ["coord-new"]);
}

/// A sweep's resume point is saved, replaced, and cleared at the end of a pass.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_sweep_cursor_is_saved_replaced_and_cleared() {
    use biffix_ingest::store::cursor::{reconcile_cursor, set_reconcile_cursor};
    let pool = common::isolated_pool("a_sweep_cursor_is_saved_replaced_and_cleared").await;
    assert_eq!(reconcile_cursor(&pool, "c:b").await.unwrap(), None);
    set_reconcile_cursor(&pool, "c:b", Some("k1")).await.unwrap();
    set_reconcile_cursor(&pool, "c:b", Some("k2")).await.unwrap();
    set_reconcile_cursor(&pool, "c:other", Some("x")).await.unwrap();
    assert_eq!(reconcile_cursor(&pool, "c:b").await.unwrap().as_deref(), Some("k2"));
    set_reconcile_cursor(&pool, "c:b", None).await.unwrap();
    assert_eq!(reconcile_cursor(&pool, "c:b").await.unwrap(), None);
    assert_eq!(reconcile_cursor(&pool, "c:other").await.unwrap().as_deref(), Some("x"));
}

/// A reconciled terminal carries the host id DescribeJobs reports, like the EventBridge one, so the
/// attempt keeps its host (and can be priced) even when its RUNNING event was also missed.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_reconciled_terminal_attributes_the_attempts_host() {
    let pool = common::isolated_pool("reconciled_terminal_attributes_host").await;
    append_submitted(&pool, "job-h", 1.0).await;
    let terminal = terminal_run_event(&DescribedTerminalJob {
        facts: DescribedJobFacts {
            instance_id: Some("ci-7".into()),
            queue: Some("q-spot".into()),
            ..Default::default()
        },
        ..described("job-h")
    });
    append(&pool, &terminal).await.unwrap();
    let (instance, queue): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT instance_id, queue FROM attempt_projection WHERE batch_job_id = 'job-h'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(instance.as_deref(), Some("ci-7"));
    assert_eq!(queue.as_deref(), Some("q-spot"));
}

/// The status probe's two projection queries, against the real schema: the unfolded-event lag
/// reads past the fold cursor, and the "stuck in submitted" count measures from the launch
/// (falling back to submission) within `(now - 2 h, now - 15 min)`. The probe is `aws`-gated.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn status_probe_queries_read_the_fold_cursor_and_the_launch_window() {
    use biffix_ingest::store::status::{stuck_run_count, unfolded_age_secs};
    let pool = common::isolated_pool("status_probe_queries").await;
    let now = biffix_core::clock::since_epoch().as_secs_f64();
    let minutes = |m: f64| now - m * 60.0;
    let anchor = |run: &str, at: f64| {
        biffix_core::event::run_submitted(
            &RunId::new(run),
            Timestamp::from_epoch_seconds(at),
            biffix_core::testing::anchor(),
        )
    };
    let started = |run: &str, at: f64| {
        testing::event(
            at,
            EventSource::Snakemake,
            format!("{run}-started"),
            EventData::RunStarted { session_id: None },
        )
        .with_run(run)
    };
    for e in [
        // Submitted 50 min ago but waited for its images and launched 1 min ago: not stuck yet
        // (it is measured from the launch, not the submission).
        common::preparing("r-waited", minutes(50.0)),
        anchor("r-waited", minutes(1.0)),
        // Launched 30 min ago and still `submitted`: stuck.
        anchor("r-stuck", minutes(30.0)),
        // Launched 30 min ago but running: not stuck.
        anchor("r-running", minutes(30.0)),
        started("r-running", minutes(29.0)),
        // Launched 3 h ago: an abandoned zombie outside the window.
        anchor("r-zombie", minutes(180.0)),
    ] {
        assert!(append(&pool, &e).await.unwrap());
    }
    assert_eq!(stuck_run_count(&pool).await.unwrap(), 1);

    // Everything appended so far is folded (each append advances the cursor).
    assert_eq!(unfolded_age_secs(&pool).await.unwrap(), None);
    // A row past the cursor (logged, not yet folded) is lag.
    let late = anchor("r-unfolded", now);
    common::log_unfolded(&pool, &late, serde_json::to_value(&late).unwrap()).await;
    let age = unfolded_age_secs(&pool).await.unwrap();
    assert!(
        matches!(age, Some(secs) if (0..60).contains(&secs)),
        "one just-logged unfolded event: {age:?}"
    );
}

/// The probe's write path, against the real schema: every row of a sample shares one `sampled_at`,
/// and its promise of the next, a duplicate component in a sample is dropped rather than failing the
/// whole write, and rows past the 100-day retention are pruned while ones inside it are kept.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn status_samples_are_written_at_one_timestamp_and_pruned_past_retention() {
    use biffix_ingest::status::Health;
    use biffix_ingest::store::status::record_samples;
    let pool = common::isolated_pool("status_samples_write").await;
    for (component, age_days) in [("old", 101), ("recent", 99)] {
        sqlx::query(
            "INSERT INTO status_samples (sampled_at, component, status) \
             VALUES (now() - make_interval(days => $1), $2, 'ok')",
        )
        .bind(age_days)
        .bind(component)
        .execute(&pool)
        .await
        .unwrap();
    }

    record_samples(
        &pool,
        &[
            ("api", Health::Ok, None),
            ("event_ingest", Health::Degraded, Some("lagging".into())),
            ("batch_compute", Health::Down, None),
            // A second row for a component at the same timestamp: dropped, not an error.
            ("api", Health::Down, Some("duplicate".into())),
        ],
        300,
    )
    .await
    .unwrap();

    // (component, status, detail, next_within_secs, whether it is the latest sample)
    type Row = (String, String, Option<String>, Option<i64>, bool);
    let got: Vec<Row> = sqlx::query_as(
        "SELECT component, status, detail, next_within_secs, \
         sampled_at = max(sampled_at) OVER () AS latest \
         FROM status_samples ORDER BY component COLLATE \"C\"",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let row = |c: &str, s: &str, d: Option<&str>, next: Option<i64>, latest: bool| {
        (c.to_string(), s.to_string(), d.map(str::to_string), next, latest)
    };
    assert_eq!(
        got,
        vec![
            row("api", "ok", None, Some(300), true),
            row("batch_compute", "down", None, Some(300), true),
            row("event_ingest", "degraded", Some("lagging"), Some(300), true),
            // `old` (101 days) is pruned; `recent` (99 days) is inside the window, and written
            // before samples carried a promise.
            row("recent", "ok", None, None, false),
        ]
    );
}

/// What DescribeJobs reports for job `job_id` started at 1.5 s on host `ci-7` (no prior attempt).
fn described_start(job_id: &str) -> DescribedStartedJob {
    DescribedStartedJob {
        batch_job_id: job_id.into(),
        started_at_millis: 1_500,
        prior_attempts: 0,
        facts: DescribedJobFacts {
            log_stream: Some(format!("batch/{job_id}")),
            instance_id: Some("ci-7".into()),
            queue: Some("q-new".into()),
            is_coordinator: false,
            requested: Default::default(),
        },
    }
}

/// Each candidate's job id and the dedup keys of the starts and execution events the log holds
/// for it.
fn starts(candidates: Vec<StaleAttempt>) -> Vec<(String, Vec<String>)> {
    candidates.into_iter().map(|c| (c.batch_job_id, c.known_fact_keys)).collect()
}

/// `(job, keys)`, owned, for comparing with [`starts`].
fn keyed(job: &str, keys: &[&str]) -> (String, Vec<String>) {
    (job.to_owned(), keys.iter().map(|&key| key.to_owned()).collect())
}

/// The backstop emits a start only while the log lacks that fact, so a job that runs for hours
/// does not enqueue a duplicate every rotation, yet a Batch retry's start is still backfilled: a
/// candidate carries the dedup keys of the AWS starts of either shape its log holds.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_stale_candidate_carries_the_keys_of_the_starts_the_log_holds() {
    let pool =
        common::isolated_pool("a_stale_candidate_carries_the_keys_of_the_starts_the_log_holds")
            .await;
    let batch = |job: &str, ts: f64, data: EventData| {
        testing::event(ts, EventSource::AwsBatch, format!("aws:{job}:{ts}"), data).with_job(job)
    };
    for job in ["job-task-start", "job-coord-start", "job-none", "job-retried"] {
        append_submitted(&pool, job, 1.0).await;
    }
    // Two starts (a STARTING and a retry's RUNNING, say): every key comes back, in order.
    for ts in [1.3, 1.1] {
        let started = EventData::TaskStarted { started_at: None, log_stream: None };
        append(&pool, &batch("job-retried", ts, started)).await.unwrap();
    }
    append(
        &pool,
        &batch(
            "job-task-start",
            1.2,
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
    )
    .await
    .unwrap();
    // A task on a listed coordinator queue: its start arrives coordinator-shaped.
    append(
        &pool,
        &batch("job-coord-start", 1.2, EventData::CoordinatorRunning { log_stream: None }),
    )
    .await
    .unwrap();
    // An engine's own start is not Batch's.
    append(
        &pool,
        &event(
            "job-none",
            1.2,
            "engine:job-none:started",
            EventSource::Snakemake,
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        starts(stale_attempts(&pool, window(2_000_000, 100, 0)).await.unwrap()),
        [
            keyed("job-coord-start", &["aws:job-coord-start:1.2"]),
            keyed("job-none", &[]),
            keyed("job-task-start", &["aws:job-task-start:1.2"]),
            // Last: its newest observation (1.3) is the latest.
            keyed("job-retried", &["aws:job-retried:1.1", "aws:job-retried:1.3"]),
        ]
    );

    append_bound(&pool, "coord-1", 1.0, "cp:coord-1:sub").await;
    assert_eq!(
        starts(stale_coordinators(&pool, window(2_000_000, 100, 0)).await.unwrap()),
        [keyed("coord-1", &[])]
    );
    append(&pool, &batch("coord-1", 1.5, EventData::CoordinatorRunning { log_stream: None }))
        .await
        .unwrap();
    assert_eq!(
        starts(stale_coordinators(&pool, window(2_000_000, 100, 0)).await.unwrap()),
        [keyed("coord-1", &["aws:coord-1:1.5"])]
    );
}

/// A candidate of either path carries the keys of the execution events the log holds beside its
/// starts', so the backstop enqueues a described execution once rather than on every rotation.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_candidate_carries_the_execution_keys_the_log_holds() {
    let pool = common::isolated_pool("a_candidate_carries_the_execution_keys_the_log_holds").await;
    append_submitted(&pool, "job-k", 1.0).await;
    let started = EventData::TaskStarted { started_at: None, log_stream: None };
    let start = testing::event(1.2, EventSource::AwsBatch, "aws-batch:job-k:RUNNING", started);
    append(&pool, &start.with_job("job-k")).await.unwrap();
    append(&pool, &testing::execution_started("job-k", "t1", 1.3)).await.unwrap();
    // An engine's own event about the job is not one of Batch's facts.
    append(
        &pool,
        &event(
            "job-k",
            1.4,
            "engine:job-k:started",
            EventSource::Snakemake,
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
    )
    .await
    .unwrap();

    let held = [keyed("job-k", &["aws-batch-exec:t1:started", "aws-batch:job-k:RUNNING"])];
    assert_eq!(starts(stale_attempts(&pool, window(2_000_000, 100, 0)).await.unwrap()), held);
    let live = live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap();
    let children: Vec<(String, Vec<String>)> =
        live.into_iter().map(|c| (c.batch_job_id, c.known_fact_keys)).collect();
    assert_eq!(children, held);
}

/// The reaper filters a described child's start by the starts the log holds, as the stale path
/// does: each live child carries the dedup keys of its AWS Batch starts of either shape, and an
/// engine's own start is not one.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_live_child_carries_the_keys_of_the_starts_the_log_holds() {
    let pool =
        common::isolated_pool("a_live_child_carries_the_keys_of_the_starts_the_log_holds").await;
    let batch = |job: &str, ts: f64, data: EventData| {
        testing::event(ts, EventSource::AwsBatch, format!("aws:{job}:{ts}"), data).with_job(job)
    };
    for job in ["job-a", "job-b", "job-c", "job-done"] {
        append_submitted(&pool, job, 1.0).await;
    }
    // job-a: a first attempt's start and a retry's, both Batch's.
    for ts in [1.3, 1.1] {
        let started = EventData::TaskStarted { started_at: None, log_stream: None };
        append(&pool, &batch("job-a", ts, started)).await.unwrap();
    }
    // job-b: its start arrived coordinator-shaped (a listed coordinator queue).
    append(&pool, &batch("job-b", 1.2, EventData::CoordinatorRunning { log_stream: None }))
        .await
        .unwrap();
    // job-c: only the engine's own start, which is not Batch's.
    append(
        &pool,
        &event(
            "job-c",
            1.2,
            "engine:job-c:started",
            EventSource::Snakemake,
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
    )
    .await
    .unwrap();
    // job-done: started and ended, so it is not live.
    append(
        &pool,
        &batch("job-done", 1.2, EventData::TaskStarted { started_at: None, log_stream: None }),
    )
    .await
    .unwrap();
    append_succeeded(&pool, "job-done", 2.0).await;

    let live = live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap();
    let children: Vec<(String, Vec<String>)> =
        live.into_iter().map(|c| (c.batch_job_id, c.known_fact_keys)).collect();
    assert_eq!(
        children,
        [
            keyed("job-a", &["aws:job-a:1.1", "aws:job-a:1.3"]),
            keyed("job-b", &["aws:job-b:1.2"]),
            keyed("job-c", &[]),
        ]
    );
    // A later page carries its own children's keys.
    let paged = live_children_of_run(&pool, "run-reconcile", Some("job-a"), 1).await.unwrap();
    assert_eq!(
        paged,
        [LiveChild { batch_job_id: "job-b".into(), known_fact_keys: vec!["aws:job-b:1.2".into()] }]
    );
}

/// A running job whose live events never arrived (its queue unrouted) shows running on its host
/// once the backstop enqueues the start DescribeJobs reports, and the live RUNNING, should it come
/// after all, collapses onto it.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_reconciled_start_shows_a_running_job_on_its_host() {
    use biffix_core::eventbridge::decode::{decode_batch, QueueContext};
    let pool = common::isolated_pool("a_reconciled_start_shows_a_running_job_on_its_host").await;
    append_submitted(&pool, "job-r", 1.0).await;
    let start = started_run_event(&described_start("job-r"));
    assert!(append(&pool, &start).await.unwrap());

    let (state, started_at, instance): (String, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT state, started_at, instance_id FROM attempt_projection \
         WHERE batch_job_id = 'job-r'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "running");
    assert_eq!(started_at, Some(1_500_000));
    assert_eq!(instance.as_deref(), Some("ci-7"));
    let task_state: String = sqlx::query_scalar(
        "SELECT state FROM task_projection \
         WHERE run_id = 'run-reconcile' AND task_key = 'align:sample=job-r'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(task_state, "running");

    // The next rotation's start, and the late live RUNNING, are the same fact.
    assert!(!append(&pool, &start).await.unwrap());
    let live = decode_batch(
        &serde_json::json!({
            "time": "2026-09-12T04:12:32Z",
            "detail": {
                "jobId": "job-r",
                "jobQueue": "q/q-new",
                "status": "RUNNING",
                "startedAt": 1_500,
                "attempts": [],
                "container": {
                    "logStreamName": "batch/job-r",
                    "containerInstanceArn": "arn:aws:ecs:us-east-1:1:container-instance/c/ci-7"
                }
            }
        }),
        &QueueContext { coordinator_queues: vec![] },
    )
    .unwrap()
    .remove(0);
    assert!(!append(&pool, &live).await.unwrap());
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM run_events WHERE batch_job_id = 'job-r'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 2, "the submit and one start");

    // Its terminal is still owed, so it stays a candidate, now holding its start's key.
    assert_eq!(
        starts(stale_attempts(&pool, window(2_000_000, 100, 0)).await.unwrap()),
        [keyed("job-r", &[start.dedup_key.as_str()])]
    );
}

/// A backfilled terminal alone leaves the attempt without a start, so the pricer cannot price it;
/// with the backfilled start (here folded after the terminal) it has a run time and is priced.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_reconciled_terminal_with_its_start_has_a_run_time() {
    use biffix_ingest::store::pricing::{unpriced_jobs, JobRole};
    let pool = common::isolated_pool("a_reconciled_terminal_with_its_start_has_a_run_time").await;
    sqlx::query(
        "INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
         instance_type, availability_zone, detail_version, raw_detail, first_seen_at, lifecycle) \
         VALUES ('ci-7', 'i-7', 'arn:cluster', 'm6i.large', 'us-east-1a', 1, '{}', 0, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    append_submitted(&pool, "job-p", 1.0).await;
    let terminal = terminal_run_event(&DescribedTerminalJob {
        facts: DescribedJobFacts {
            instance_id: Some("ci-7".into()),
            queue: Some("q-new".into()),
            ..Default::default()
        },
        ..described("job-p")
    });
    assert!(append(&pool, &terminal).await.unwrap());
    assert!(
        unpriced_jobs(&pool, JobRole::Task, 0, None, 100).await.unwrap().is_empty(),
        "a terminal without its start has no run time to price"
    );

    assert!(append(&pool, &started_run_event(&described_start("job-p"))).await.unwrap());
    let (state, started_at, finished_at, instance): (
        String,
        Option<i64>,
        Option<i64>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT state, started_at, finished_at, instance_id FROM attempt_projection \
         WHERE batch_job_id = 'job-p'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "succeeded");
    assert_eq!(started_at, Some(1_500_000));
    assert_eq!(finished_at, Some(2_000_000));
    assert_eq!(instance.as_deref(), Some("ci-7"));
    let priced: Vec<String> = unpriced_jobs(&pool, JobRole::Task, 0, None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|job| job.batch_job_id)
        .collect();
    assert_eq!(priced, ["job-p"]);
}

/// AWS Batch's start (`edge` "started") or end ("ended") of execution `n` of `job_id`, at `ts`,
/// under `aws-batch-exec:{job_id}-t{n}:{edge}`.
async fn append_execution(pool: &sqlx::PgPool, job_id: &str, n: u32, edge: &str, ts: f64) {
    let execution_id = format!("{job_id}-t{n}");
    let event = match edge {
        "started" => testing::execution_started(job_id, &execution_id, ts),
        "ended" => testing::execution_ended(job_id, &execution_id, None, ts, None),
        other => panic!("no such execution edge: {other}"),
    };
    assert_eq!(event.dedup_key, format!("aws-batch-exec:{execution_id}:{edge}"));
    append(pool, &event).await.unwrap();
}

/// A task attempt bound at 100 s whose SUCCEEDED Batch reported at `ended_s`.
async fn append_finished(pool: &sqlx::PgPool, job_id: &str, ended_s: f64) {
    append_submitted(pool, job_id, 100.0).await;
    append_succeeded(pool, job_id, ended_s).await;
}

/// AWS Batch's RUNNING for job `job_id` at `ts`: the job's own start, not an execution's.
async fn append_job_started(pool: &sqlx::PgPool, job_id: &str, ts: f64) {
    append_retry_started(pool, job_id, 0, ts).await;
}

/// Batch's start of `job_id`'s execution after `prior` others (its RUNNING is keyed per execution).
async fn append_retry_started(pool: &sqlx::PgPool, job_id: &str, prior: u32, ts: f64) {
    let started = EventData::TaskStarted { started_at: None, log_stream: None };
    let key = match prior {
        0 => format!("aws-batch:{job_id}:RUNNING"),
        prior => format!("aws-batch:{job_id}:RUNNING:{prior}"),
    };
    let start = testing::event(ts, EventSource::AwsBatch, key, started).with_job(job_id);
    append(pool, &start).await.unwrap();
}

/// The execution candidates' job ids, in the query's order, among the jobs bound since 50 s.
async fn execution_candidate_ids(pool: &sqlx::PgPool, limit: i64) -> Vec<String> {
    use biffix_ingest::store::reconcile::execution_candidates;
    let candidates = execution_candidates(pool, 50_000_000, limit).await.unwrap();
    candidates.into_iter().map(|candidate| candidate.batch_job_id).collect()
}

/// `(executions_described_micros, missing_since_micros)` of `job_id`'s `reconcile_describe` row.
async fn describe_marks(pool: &sqlx::PgPool, job_id: &str) -> (Option<i64>, Option<i64>) {
    sqlx::query_as(
        "SELECT executions_described_micros, missing_since_micros FROM reconcile_describe \
         WHERE batch_job_id = $1",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The third candidate class: a finished job, task or coordinator, bound in the window, whose log
/// holds no execution end, fewer ends than starts, or no start of the job. A job whose record is
/// complete, one still running and one that finished before the window are not asked about (one
/// bound before it but finished inside it is), one described without being marked is asked
/// again, and only AWS Batch's own events count.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn execution_candidates_are_finished_jobs_missing_executions_or_a_start() {
    use biffix_ingest::store::reconcile::execution_candidates;
    let pool = common::isolated_pool(
        "execution_candidates_are_finished_jobs_missing_executions_or_a_start",
    )
    .await;
    // Finished, and the log holds neither its start nor any execution.
    append_finished(&pool, "job-none", 110.0).await;
    // Finished, with its start, and an execution that started and has no recorded end.
    append_finished(&pool, "job-open", 110.0).await;
    append_job_started(&pool, "job-open", 101.0).await;
    append_execution(&pool, "job-open", 1, "started", 101.0).await;
    // Finished, with its start, two executions started and only one ended.
    append_finished(&pool, "job-short", 110.0).await;
    append_job_started(&pool, "job-short", 101.0).await;
    append_execution(&pool, "job-short", 1, "started", 101.0).await;
    append_execution(&pool, "job-short", 1, "ended", 103.0).await;
    append_execution(&pool, "job-short", 2, "started", 105.0).await;
    // Finished, with its start and no execution at all.
    append_finished(&pool, "job-start-only", 110.0).await;
    append_job_started(&pool, "job-start-only", 101.0).await;
    // Finished with every execution ended (its terminal carried both ends), but its start was
    // lost: nothing else would ever ask about it.
    append_finished(&pool, "job-no-start", 110.0).await;
    append_execution(&pool, "job-no-start", 1, "ended", 103.0).await;
    append_execution(&pool, "job-no-start", 2, "ended", 109.0).await;
    // Its retry's start arrived but the first execution's was lost, so the attempt's span starts
    // at the retry: two ends, one start of the job.
    append_finished(&pool, "job-first-start-lost", 110.0).await;
    append_retry_started(&pool, "job-first-start-lost", 1, 105.0).await;
    append_execution(&pool, "job-first-start-lost", 1, "ended", 103.0).await;
    append_execution(&pool, "job-first-start-lost", 2, "ended", 109.0).await;
    // The same as job-no-start, where only the engine reported a start: an engine's start is not
    // Batch's.
    append_finished(&pool, "job-engine-start", 110.0).await;
    append_execution(&pool, "job-engine-start", 1, "ended", 109.0).await;
    let engine_started = EventData::TaskStarted { started_at: None, log_stream: None };
    append(
        &pool,
        &event(
            "job-engine-start",
            101.0,
            "engine:job-engine-start:started",
            EventSource::Snakemake,
            engine_started,
        ),
    )
    .await
    .unwrap();
    // Complete: its start, and every execution that started has ended.
    append_finished(&pool, "job-complete", 110.0).await;
    append_job_started(&pool, "job-complete", 101.0).await;
    append_execution(&pool, "job-complete", 1, "started", 101.0).await;
    append_execution(&pool, "job-complete", 1, "ended", 109.0).await;
    // Complete: each execution's start of the job and its end alone, which is what a finished
    // job's describe yields.
    append_finished(&pool, "job-ended-only", 110.0).await;
    append_job_started(&pool, "job-ended-only", 101.0).await;
    append_retry_started(&pool, "job-ended-only", 1, 105.0).await;
    append_execution(&pool, "job-ended-only", 1, "ended", 103.0).await;
    append_execution(&pool, "job-ended-only", 2, "ended", 109.0).await;
    // Has its start and Batch's execution end, so complete: an execution start an engine claims
    // is not Batch's and leaves no end owed.
    append_finished(&pool, "job-untrusted", 110.0).await;
    append_job_started(&pool, "job-untrusted", 101.0).await;
    append_execution(&pool, "job-untrusted", 1, "ended", 109.0).await;
    for n in [8, 9] {
        let claimed = EventData::BatchExecutionStarted {
            execution_id: format!("job-untrusted-t{n}"),
            started_at: Timestamp::from_epoch_seconds(102.0),
            log_stream: None,
        };
        let key = format!("engine:job-untrusted:exec-{n}");
        let claimed = testing::event(102.0, EventSource::Snakemake, key, claimed);
        append(&pool, &claimed.with_job("job-untrusted")).await.unwrap();
    }
    // Not finished: the no-terminal class owns it.
    append_submitted(&pool, "job-running", 100.0).await;
    append_execution(&pool, "job-running", 1, "started", 101.0).await;
    // Finished like job-none, but before the window: Batch no longer has it.
    append_submitted(&pool, "job-old", 10.0).await;
    append_succeeded(&pool, "job-old", 40.0).await;
    // Bound before the window but finished inside it (it ran long): Batch still has it.
    append_submitted(&pool, "job-long", 10.0).await;
    append_succeeded(&pool, "job-long", 110.0).await;
    // Complete by every count, but an earlier describe did not mark it (an event it sent was
    // rejected): asked again until it is marked.
    append_finished(&pool, "job-touched", 110.0).await;
    append_job_started(&pool, "job-touched", 101.0).await;
    append_execution(&pool, "job-touched", 1, "ended", 109.0).await;
    biffix_ingest::store::reconcile::touch_described(&pool, &["job-touched".into()], 1)
        .await
        .unwrap();
    // A finished coordinator with no executions.
    append_bound(&pool, "coord-1", 100.0, "cp:coord-1:sub").await;
    let succeeded = testing::event(
        110.0,
        EventSource::AwsBatch,
        "aws-batch:coord-1:SUCCEEDED",
        EventData::CoordinatorSucceeded { log_stream: None },
    );
    append(&pool, &succeeded.with_job("coord-1")).await.unwrap();

    let mut candidates = execution_candidates(&pool, 50_000_000, 100).await.unwrap();
    candidates.sort_by(|a, b| a.batch_job_id.cmp(&b.batch_job_id));
    let found: Vec<(&str, bool)> =
        candidates.iter().map(|c| (c.batch_job_id.as_str(), c.bound_coordinator)).collect();
    assert_eq!(
        found,
        [
            ("coord-1", true),
            ("job-engine-start", false),
            ("job-first-start-lost", false),
            ("job-long", false),
            ("job-no-start", false),
            ("job-none", false),
            ("job-open", false),
            ("job-short", false),
            ("job-start-only", false),
            ("job-touched", false),
        ]
    );
    // Each carries the keys of the facts the log already holds for it.
    let keys = |job: &str| {
        let candidate = candidates.iter().find(|c| c.batch_job_id == job).unwrap();
        candidate.known_fact_keys.clone()
    };
    assert!(keys("coord-1").is_empty() && keys("job-none").is_empty());
    assert_eq!(
        keys("job-no-start"),
        ["aws-batch-exec:job-no-start-t1:ended", "aws-batch-exec:job-no-start-t2:ended"]
    );
    assert_eq!(
        keys("job-open"),
        ["aws-batch-exec:job-open-t1:started", "aws-batch:job-open:RUNNING"]
    );
    assert_eq!(
        keys("job-short"),
        [
            "aws-batch-exec:job-short-t1:ended",
            "aws-batch-exec:job-short-t1:started",
            "aws-batch-exec:job-short-t2:started",
            "aws-batch:job-short:RUNNING",
        ]
    );
}

/// The newest-finished candidates come first, so current runs are completed before a backlog; a
/// failed describe or send rotates a job behind the ones not yet tried; and a marked job is never
/// asked about again.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn execution_candidates_come_newest_first_rotate_and_stop_once_marked() {
    use biffix_ingest::store::reconcile::{mark_executions_described, touch_described};
    let pool =
        common::isolated_pool("execution_candidates_come_newest_first_rotate_and_stop_once_marked")
            .await;
    for (job, ended_s) in [("j120", 120.0), ("j110", 110.0), ("j130", 130.0)] {
        append_finished(&pool, job, ended_s).await;
    }
    let now = 500_000_000;
    assert_eq!(execution_candidate_ids(&pool, 2).await, ["j130", "j120"]);

    touch_described(&pool, &["j130".into()], now).await.unwrap();
    assert_eq!(execution_candidate_ids(&pool, 2).await, ["j120", "j110"]);

    mark_executions_described(&pool, &["j120".into()], now).await.unwrap();
    assert_eq!(execution_candidate_ids(&pool, 2).await, ["j110", "j130"]);
    assert!(execution_candidate_ids(&pool, 0).await.is_empty());
}

/// A finished job that never got an execution (cancelled before it reached a host, so Batch's
/// `attempts[]` is empty) has no end to wait for: it is described once and then left alone,
/// rather than on every poll for the rest of its window.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_finished_job_with_no_executions_is_described_once() {
    use biffix_core::eventbridge::execution::BatchJobSnapshot;
    use biffix_ingest::reconcile::{execution_backfill, execution_marks};
    use biffix_ingest::store::reconcile::{mark_executions_described, record_describe_results};
    use std::collections::{BTreeSet, HashMap};
    let pool = common::isolated_pool("a_finished_job_with_no_executions_is_described_once").await;
    // What Batch says of a job cancelled from RUNNABLE: finished, with no execution and no start.
    let finished_snapshot = BatchJobSnapshot {
        batch_job_id: "job-none".into(),
        status: biffix_core::wire::aws_batch::BatchStatus::Failed,
        started_at_millis: None,
        current: Default::default(),
        ..testing::retried_job_snapshot()
    };
    append_finished(&pool, "job-none", 110.0).await;
    assert_eq!(execution_candidate_ids(&pool, 100).await, ["job-none"]);

    // Batch returned the job with nothing to send, so nothing was rejected.
    let asked = ["job-none".to_string()];
    let backfill = execution_backfill(
        [("job-none", vec![], Some(BatchJobSnapshot { attempts: vec![], ..finished_snapshot }))],
        &HashMap::from([("job-none", &[][..])]),
    );
    assert!(backfill.events.is_empty(), "{:?}", backfill.events);
    let marks = execution_marks(&asked, &backfill, &BTreeSet::new());
    assert_eq!(marks.described, asked);
    let now = 500_000_000;
    mark_executions_described(&pool, &marks.described, now).await.unwrap();

    assert!(execution_candidate_ids(&pool, 100).await.is_empty());
    assert_eq!(describe_marks(&pool, "job-none").await, (Some(now), None));

    // A later describe of the job by another path (and its cleanup) leaves the mark set.
    record_describe_results(&pool, &asked, &[], now + 1, now - 1).await.unwrap();
    assert_eq!(describe_marks(&pool, "job-none").await, (Some(now), None));
    assert!(execution_candidate_ids(&pool, 100).await.is_empty());
}

/// Marking a job Batch has been omitting keeps since when: the stale backstop's give-up and the
/// reaper's forgotten-child rule both read that streak.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn marking_keeps_an_omission_streak() {
    use biffix_ingest::store::reconcile::{mark_executions_described, record_describe_results};
    let pool = common::isolated_pool("marking_keeps_an_omission_streak").await;
    record_describe_results(&pool, &[], &["job-gone".into()], 500, 0).await.unwrap();

    mark_executions_described(&pool, &["job-gone".into()], 600).await.unwrap();

    assert_eq!(describe_marks(&pool, "job-gone").await, (Some(600), Some(500)));
    // Nothing to mark is not an error.
    mark_executions_described(&pool, &[], 700).await.unwrap();
}

/// A retried job whose live events were all lost but its SUCCEEDED: DescribeJobs alone gives back
/// both executions and each execution's start of the job, each event surviving the ingest queue's
/// encoding, and the job then needs nothing more.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn describe_jobs_alone_recovers_a_retried_jobs_executions() {
    use biffix_core::eventbridge::decode::QueueContext;
    use biffix_core::eventbridge::execution::BatchJobSnapshot;
    use biffix_core::eventbridge::{route, Ingested, NormalizedIngestEnvelope};
    use biffix_ingest::reconcile::described_events;
    use biffix_ingest::store::reconcile::{execution_candidates, known_fact_keys_of};
    let pool =
        common::isolated_pool("describe_jobs_alone_recovers_a_retried_jobs_executions").await;
    let bound = |ts: f64, source: EventSource, key: &str, data: EventData| {
        testing::event(ts, source, key, data)
            .with_run("r1")
            .with_task("align")
            .with_attempt(1)
            .with_job("job-1")
    };
    let submitted = EventData::TaskSubmitted { image: None };
    append(&pool, &bound(1.0, EventSource::Snakemake, "engine:job-1:submitted", submitted))
        .await
        .unwrap();
    let succeeded = EventData::TaskSucceeded { exit_code: 0, log_stream: None };
    append(&pool, &bound(2217.0, EventSource::AwsBatch, "aws-batch:job-1:SUCCEEDED", succeeded))
        .await
        .unwrap();

    let candidates = execution_candidates(&pool, 0, 100).await.unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!((candidate.batch_job_id.as_str(), candidate.bound_coordinator), ("job-1", false));
    assert!(candidate.known_fact_keys.is_empty());

    // What DescribeJobs says of the finished job: each execution's start (`described_starts`)
    // and both executions.
    let start = |started_at_millis: i64, prior_attempts: u32, host: &str| DescribedStartedJob {
        batch_job_id: "job-1".into(),
        started_at_millis,
        prior_attempts,
        facts: DescribedJobFacts {
            instance_id: Some(host.into()),
            queue: Some(testing::RETRIED_QUEUE.into()),
            ..Default::default()
        },
    };
    let starts = [start(3_000, 0, "ci1"), start(831_000, 1, "ci2")];
    let snapshot =
        BatchJobSnapshot { batch_job_id: "job-1".into(), ..testing::retried_job_snapshot() };
    let events = described_events(&starts, Some(&snapshot), None, &candidate.known_fact_keys);
    assert_eq!(events.len(), 4, "each execution's start and end: {events:?}");

    // Each goes the way the tick sends it: wrapped in the ingest envelope, onto the queue, and
    // through the drain's router to the single append path.
    let context = QueueContext { coordinator_queues: vec![] };
    for event in &events {
        let message = serde_json::to_string(&NormalizedIngestEnvelope::new(event.clone())).unwrap();
        let mut routed = route(&serde_json::from_str(&message).unwrap(), &context).unwrap();
        assert_eq!(routed.len(), 1, "one message, one event");
        let Some(Ingested::Event(decoded)) = routed.pop() else {
            panic!("a reconciled event routes as an event");
        };
        assert_eq!(&decoded, event, "the queue encoding loses nothing");
        assert!(append(&pool, &decoded).await.unwrap());
    }

    // (instance_id, ended, termination_category) of the job's executions, in order.
    type Execution = (Option<String>, bool, Option<String>);
    let executions: Vec<Execution> = sqlx::query_as(
        "SELECT instance_id, ended, termination_category FROM execution_projection \
         WHERE batch_job_id = 'job-1' ORDER BY ordinal",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        executions,
        [
            (Some("ci1".to_string()), true, Some("host_terminated".to_string())),
            (Some("ci2".to_string()), true, None),
        ]
    );
    // (started_at, finished_at, run_micros, lost_micros, wait_micros)
    type Span = (Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>);
    let span: Span = sqlx::query_as(
        "SELECT started_at, finished_at, run_micros, lost_micros, wait_micros \
         FROM attempt_projection WHERE batch_job_id = 'job-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        span,
        (
            // The start the log lacked is repaired: the first execution's.
            Some(3_000_000),
            Some(2_217_000_000),
            Some(2_067_000_000),
            Some(681_000_000),
            Some(147_000_000),
        )
    );

    // Complete now, so it is no longer a candidate, without having been marked.
    assert!(execution_candidates(&pool, 0, 100).await.unwrap().is_empty());
    let marked: i64 = sqlx::query_scalar("SELECT count(*) FROM reconcile_describe")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(marked, 0);
    // And a second describe would find nothing the log lacks.
    let mut known = known_fact_keys_of(&pool, &["job-1".to_string()]).await.unwrap();
    let known = known.remove("job-1").unwrap_or_default();
    assert_eq!(known.len(), 4);
    assert!(described_events(&starts, Some(&snapshot), None, &known).is_empty());
}

/// A finished job whose terminal carried every execution's end while its start was lost: no
/// execution is owed, but nothing else asks about a finished job, so the missing start alone makes
/// it a candidate. One describe backfills the start and the job is marked.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_finished_job_whose_start_was_lost_is_described_once_for_it() {
    use biffix_core::eventbridge::execution::{execution_events, BatchJobSnapshot};
    use biffix_ingest::reconcile::{execution_backfill, execution_marks};
    use biffix_ingest::store::reconcile::{execution_candidates, mark_executions_described};
    use std::collections::{BTreeSet, HashMap};
    let pool =
        common::isolated_pool("a_finished_job_whose_start_was_lost_is_described_once_for_it").await;
    append_submitted(&pool, "job-s", 1.0).await;
    append_succeeded(&pool, "job-s", 2217.0).await;
    // Both executions' ends are in the log, as the live terminal carries them.
    let snapshot =
        BatchJobSnapshot { batch_job_id: "job-s".into(), ..testing::retried_job_snapshot() };
    let ends = execution_events(&snapshot);
    assert_eq!(ends.len(), 2);
    for end in &ends {
        append(&pool, end).await.unwrap();
    }
    let started_at: Option<i64> = sqlx::query_scalar(
        "SELECT started_at FROM attempt_projection WHERE batch_job_id = 'job-s'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(started_at, None, "the start was lost");

    let candidates = execution_candidates(&pool, 0, 100).await.unwrap();
    assert_eq!(candidates.len(), 1, "two ends and no start is a candidate: {candidates:?}");
    let candidate = &candidates[0];
    assert_eq!(candidate.batch_job_id, "job-s");
    assert_eq!(candidate.known_fact_keys.len(), 2);

    // Its one describe: the executions are known, so only the starts are sent, one per execution
    // as the live stream would have logged them (`described_starts`): the lost execution's first.
    let start = |started_at_millis, prior_attempts| DescribedStartedJob {
        batch_job_id: "job-s".into(),
        started_at_millis,
        prior_attempts,
        facts: DescribedJobFacts::default(),
    };
    let known = HashMap::from([("job-s", candidate.known_fact_keys.as_slice())]);
    let starts = vec![start(3_000, 0), start(831_000, 1)];
    let backfill = execution_backfill([("job-s", starts, Some(snapshot))], &known);
    assert_eq!(backfill.events.len(), 2, "{:?}", backfill.events);
    for event in &backfill.events {
        assert_eq!(event.kind(), biffix_core::event::Kind::TaskStarted);
        assert!(append(&pool, event).await.unwrap());
    }
    let asked = ["job-s".to_string()];
    let marks = execution_marks(&asked, &backfill, &BTreeSet::new());
    assert_eq!(marks.described, asked);
    let now = 5_000_000_000;
    mark_executions_described(&pool, &marks.described, now).await.unwrap();

    let started_at: Option<i64> = sqlx::query_scalar(
        "SELECT started_at FROM attempt_projection WHERE batch_job_id = 'job-s'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    // The attempt's span starts at its first execution, not the last.
    assert_eq!(started_at, Some(3_000_000), "the first execution's start is backfilled");
    assert_eq!(describe_marks(&pool, "job-s").await, (Some(now), None));
    assert!(execution_candidates(&pool, 0, 100).await.unwrap().is_empty());
}
