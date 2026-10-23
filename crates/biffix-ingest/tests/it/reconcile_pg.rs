//! Reconciliation integration test against the real Postgres harness.
//!
//!   BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!     cargo test -p biffix-ingest --test it -- --ignored reconcile_pg::

use biffix_core::event::EventData;
use biffix_core::event::RunEvent;
use biffix_core::eventbridge::describe::{
    terminal_run_event, DescribedTerminalJob, TerminalStatus,
};
use biffix_core::ids::RunId;
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use biffix_ingest::store::reconcile::{
    live_children_of_run, run_id_for_coordinator, stale_attempts, stale_coordinators, StaleAttempt,
    StaleWindow,
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
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: false,
        requested: Default::default(),
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
        log_stream: Some("batch/job-stale".into()),
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
    let live = live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap();
    assert_eq!(live, ["job-1", "job-2"], "both children live before any terminal");
    // Keyset pagination: passing the first child as the cursor skips it and returns the rest, so
    // the reaper can page a run with more live children than one `limit`-sized page.
    // Asked while job-1 is still live, so an ignored cursor would return it.
    let paged = live_children_of_run(&pool, "run-reconcile", Some("job-1"), 100).await.unwrap();
    assert_eq!(paged, ["job-2"], "keyset cursor skips ids <= the cursor");

    append_succeeded(&pool, "job-1", 2.0).await;
    let after = live_children_of_run(&pool, "run-reconcile", None, 100).await.unwrap();
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
    let first = live_children_of_run(&pool, "run-paged", None, 2).await.unwrap();
    assert_eq!(first, ["child-1", "child-2"], "a full first page");
    let second = live_children_of_run(&pool, "run-paged", Some("child-2"), 2).await.unwrap();
    assert_eq!(second, ["child-3"], "the cursor resumes after the last id");
    let done = live_children_of_run(&pool, "run-paged", Some("child-3"), 2).await.unwrap();
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
        instance_id: Some("ci-7".into()),
        queue: Some("q-spot".into()),
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
