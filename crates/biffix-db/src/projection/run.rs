//! The run fold: one run's projection rows (`run_projection` and its `task_projection`,
//! `attempt_projection`, `run_output` and `notifications` rows), re-derived from its complete event
//! set.

use std::collections::BTreeMap;

use biffix_core::event::TerminationCategory;
use biffix_core::ids::InstanceId;
use biffix_core::reducer::{AttemptProjection, Projection, RunProjection};
use biffix_core::state::{AttemptState, RunState};
use sqlx::PgConnection;

use super::rows_to_events;
use crate::error::DbError;

/// Persist one run's projection — its `run_projection` row and its `task_projection`,
/// `attempt_projection` and `run_output` rows — onto `conn` (a transaction connection). `rp` is a
/// full re-fold of the run, so the child tables are made to match it exactly: rows the fold no
/// longer produces are deleted, not left behind (`projection == fold`).
async fn upsert_run(
    conn: &mut PgConnection,
    run_id: &str,
    rp: &RunProjection,
) -> Result<(), DbError> {
    // input_uris is a native Postgres text[] — bind the Vec<String> directly.
    // The column is NOT NULL; a run with no anchor yet has Null params → store an empty object.
    let params = if rp.params.is_null() { serde_json::json!({}) } else { rp.params.clone() };
    // The group-level DAG as {nodes:[id], edges:[[from,to]]}; sorted (BTreeSet) so the JSON is
    // deterministic across refolds.
    let dag = serde_json::json!({
        "nodes": rp.dag_nodes.iter().collect::<Vec<_>>(),
        "edges": rp.dag_edges.iter().map(|(f, t)| [f, t]).collect::<Vec<_>>(),
    });
    // Materialize the per-outcome task counts here so a run-list read is a plain row fetch, not a
    // per-read aggregation over task_projection.
    let counts = rp.task_counts();
    // Each task's sample (attributed once, for both the task rows and the grid) and how the run's
    // samples stand (the Lab view's "9 of 12 samples done"); no counts when no task names a sample.
    let task_samples = rp.task_samples();
    let sample_grid = rp.sample_grid_from(&task_samples);
    let sample_counts = (!sample_grid.samples.is_empty()).then(|| sample_grid.counts());
    // Serialized before the write, so a value that does not serialize fails the fold rather than
    // storing a JSON `null` in its place.
    let input_tables = rp.input_tables.as_ref().map(serde_json::to_value).transpose()?;
    let reference_bundle = rp.reference_bundle.as_ref().map(serde_json::to_value).transpose()?;
    let samples = rp.samples.as_ref().map(serde_json::to_value).transpose()?;
    // Read the stored output digest and state BEFORE the run row is overwritten with the new ones:
    // an equal digest ⇒ the output set is unchanged and its rows need no rewrite; the stored state
    // says whether the run can have a notification row to replace (see `upsert_notification`).
    let outputs_digest = rp.outputs_digest();
    let stored: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT outputs_digest, state FROM run_projection WHERE run_id = $1")
            .bind(run_id)
            .fetch_optional(&mut *conn)
            .await?;
    let (stored_digest, stored_state) = match stored {
        Some((digest, state)) => (digest, Some(state)),
        None => (None, None),
    };
    // An unknown stored state is taken as terminal, so the notification rows are reconciled.
    let was_terminal = stored_state
        .as_deref()
        .is_some_and(|s| s.parse::<RunState>().map_or(true, RunState::is_terminal));
    sqlx::query(
        r"INSERT INTO run_projection
             (run_id, state, completed, total, submitted_at, completed_at,
              submitter, submitter_label, coordinator_job_id, workflow_version,
              input_uris, params, dag,
              tasks_observed, tasks_succeeded, tasks_running, tasks_failed, tasks_cached,
              tasks_queued, output_prefix, coordinator_log_stream, coordinator_host_id,
              engine_session_id, session_mismatch, resumed_from,
              outputs_total, outputs_reused, outputs_digest, run_name, input_tables,
              reference_bundle, sample_count, retention, work_deleted_at, launched_at,
              samples, samples_total, samples_done, samples_failed, samples_declared, cancelled_by,
              experiment_id, coordinator_exit_code, coordinator_status_reason,
              coordinator_termination_category, tasks_cancelled, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,
                   $23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37,$38,$39,$40,$41,
                   $42,$43,$44,$45,$46, now())
           ON CONFLICT (run_id) DO UPDATE SET
             state = EXCLUDED.state, completed = EXCLUDED.completed,
             total = EXCLUDED.total, submitted_at = EXCLUDED.submitted_at,
             completed_at = EXCLUDED.completed_at, submitter = EXCLUDED.submitter,
             submitter_label = EXCLUDED.submitter_label,
             coordinator_job_id = EXCLUDED.coordinator_job_id,
             workflow_version = EXCLUDED.workflow_version, input_uris = EXCLUDED.input_uris,
             params = EXCLUDED.params, dag = EXCLUDED.dag,
             tasks_observed = EXCLUDED.tasks_observed, tasks_succeeded = EXCLUDED.tasks_succeeded,
             tasks_running = EXCLUDED.tasks_running, tasks_failed = EXCLUDED.tasks_failed,
             tasks_cached = EXCLUDED.tasks_cached, tasks_queued = EXCLUDED.tasks_queued,
             output_prefix = EXCLUDED.output_prefix,
             coordinator_log_stream = EXCLUDED.coordinator_log_stream,
             coordinator_host_id = EXCLUDED.coordinator_host_id,
             engine_session_id = EXCLUDED.engine_session_id,
             session_mismatch = EXCLUDED.session_mismatch,
             resumed_from = EXCLUDED.resumed_from,
             outputs_total = EXCLUDED.outputs_total, outputs_reused = EXCLUDED.outputs_reused,
             outputs_digest = EXCLUDED.outputs_digest, run_name = EXCLUDED.run_name,
             input_tables = EXCLUDED.input_tables,
             reference_bundle = EXCLUDED.reference_bundle,
             sample_count = EXCLUDED.sample_count,
             retention = EXCLUDED.retention, work_deleted_at = EXCLUDED.work_deleted_at,
             launched_at = EXCLUDED.launched_at, samples = EXCLUDED.samples,
             samples_total = EXCLUDED.samples_total, samples_done = EXCLUDED.samples_done,
             samples_failed = EXCLUDED.samples_failed,
             samples_declared = EXCLUDED.samples_declared,
             cancelled_by = EXCLUDED.cancelled_by, experiment_id = EXCLUDED.experiment_id,
             coordinator_exit_code = EXCLUDED.coordinator_exit_code,
             coordinator_status_reason = EXCLUDED.coordinator_status_reason,
             coordinator_termination_category = EXCLUDED.coordinator_termination_category,
             tasks_cancelled = EXCLUDED.tasks_cancelled,
             updated_at = now()",
    )
    .bind(run_id)
    .bind(rp.state().as_str())
    .bind(rp.completed.map(|c| c as i64))
    .bind(rp.total.map(|t| t as i64))
    .bind(rp.submitted_at_micros)
    .bind(rp.completed_at_micros)
    .bind(rp.submitter.as_deref())
    .bind(rp.submitter_label.as_deref())
    .bind(rp.coordinator_job_id.as_deref())
    .bind(rp.workflow_version_id.as_deref())
    .bind(&rp.input_uris)
    .bind(&params)
    .bind(&dag)
    .bind(counts.observed as i64)
    .bind(counts.succeeded as i64)
    .bind(counts.running as i64)
    .bind(counts.failed as i64)
    .bind(counts.cached as i64)
    .bind(counts.queued as i64)
    .bind(rp.output_prefix.as_deref())
    .bind(rp.coordinator_log_stream.as_deref())
    .bind(rp.coordinator_host_id.as_deref())
    .bind(rp.effective_session_id())
    .bind(rp.session_mismatch())
    .bind(rp.resumed_from.as_deref())
    .bind(rp.outputs_total() as i64)
    .bind(rp.outputs_reused() as i64)
    .bind(&outputs_digest)
    .bind(rp.run_name.as_deref())
    .bind(input_tables)
    .bind(reference_bundle)
    .bind(rp.sample_count.map(|n| n as i32))
    .bind(rp.retention.as_ref().map(biffix_core::event::Retention::as_str))
    .bind(rp.work_deleted_at_micros)
    .bind(rp.launched_at_micros)
    .bind(samples)
    .bind(sample_counts.map(|c| c.total as i32))
    .bind(sample_counts.map(|c| c.done as i32))
    .bind(sample_counts.map(|c| c.failed as i32))
    .bind(sample_counts.map(|c| c.declared))
    .bind(rp.cancelled_by())
    .bind(rp.experiment_id())
    .bind(rp.coordinator_exit_code)
    .bind(rp.coordinator_status_reason.as_deref())
    .bind(rp.coordinator_termination_category.as_deref())
    .bind(counts.cancelled as i64)
    .execute(&mut *conn)
    .await?;
    upsert_notification(conn, run_id, rp, was_terminal).await?;

    // Rewrite the output rows only when they can differ from the fold: the digest changed, or the
    // stored row count differs from the fold's (a row written or removed outside the fold, which
    // the digest of the fold alone cannot see).
    let stored_outputs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM run_output WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&mut *conn)
            .await?;
    if stored_digest.as_deref() != Some(outputs_digest.as_str())
        || stored_outputs != rp.outputs.len() as i64
    {
        upsert_outputs(conn, run_id, rp).await?;
    }

    upsert_tasks(conn, run_id, rp, &task_samples).await?;
    // The fold is a full replay of the run's events, so its task set is complete: drop any row it
    // no longer produces (e.g. a reducer fix re-keyed or dropped a task), so the persisted
    // projection equals the fold rather than a superset of every fold ever run.
    let task_keys: Vec<&str> = rp.tasks.keys().map(String::as_str).collect();
    // An anti-join against the unnested key set (hashable), not `NOT (x = ANY($2))`, which is a
    // linear array scan per row — quadratic on a run with thousands of tasks, on every advance.
    sqlx::query(
        "DELETE FROM task_projection t WHERE t.run_id = $1 \
           AND NOT EXISTS (SELECT 1 FROM UNNEST($2::text[]) AS k(key) WHERE k.key = t.task_key)",
    )
    .bind(run_id)
    .bind(&task_keys)
    .execute(&mut *conn)
    .await?;
    upsert_attempts(conn, run_id, rp).await
}

/// Make the run's `notifications` rows match the fold ([`RunProjection::notification`]): at most
/// one, for its terminal state, addressed to its submitter. Rows the fold no longer produces (the
/// run's state changed, its submitter is not a user) are deleted, and the one it does produce is
/// upserted under its deterministic id, so a redelivery, an advance and a refold all leave the same
/// row (`projection == fold`). Read and dismissal state (`notification_reads`,
/// `notification_dismissals`) is user state and is never touched.
///
/// `was_terminal` is whether the run's stored `run_projection` row (before this upsert) was in a
/// terminal state. A run that is not terminal now and was not before writes nothing: the
/// notification rows are written only here, in the same transaction as the run row, so a run whose
/// stored row is not terminal has none, and a fold that is not terminal produces none, so they
/// already match. That skips both round trips on every advance of a running run (the common case)
/// at the cost of reading `state` alongside the digest the caller already reads. A run that was
/// terminal (and may now be resumed) or is terminal now is always reconciled.
async fn upsert_notification(
    conn: &mut PgConnection,
    run_id: &str,
    rp: &RunProjection,
    was_terminal: bool,
) -> Result<(), DbError> {
    if !was_terminal && !rp.state().is_terminal() {
        return Ok(());
    }
    let notification = rp.notification();
    sqlx::query("DELETE FROM notifications WHERE run_id = $1 AND id IS DISTINCT FROM $2")
        .bind(run_id)
        .bind(notification.as_ref().map(|n| n.id.as_str()))
        .execute(&mut *conn)
        .await?;
    let Some(n) = notification else {
        return Ok(());
    };
    sqlx::query(
        r"INSERT INTO notifications (id, user_sub, run_id, kind, at) VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (id) DO UPDATE SET
             user_sub = EXCLUDED.user_sub, run_id = EXCLUDED.run_id, kind = EXCLUDED.kind,
             at = EXCLUDED.at
           WHERE (notifications.user_sub, notifications.run_id, notifications.kind,
                  notifications.at)
                 IS DISTINCT FROM (EXCLUDED.user_sub, EXCLUDED.run_id, EXCLUDED.kind, EXCLUDED.at)",
    )
    .bind(&n.id)
    .bind(&n.user_sub)
    .bind(run_id)
    .bind(n.kind.as_str())
    .bind(n.at_micros)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Upsert every task the fold produced into `task_projection`, in one `UNNEST` statement (like
/// [`upsert_attempts`] and [`upsert_outputs`]) rather than one round trip per task: a run with
/// thousands of tasks is re-folded on every batch that touches it, under the projection lock. The
/// caller deletes the rows the fold no longer produces.
async fn upsert_tasks(
    conn: &mut PgConnection,
    run_id: &str,
    rp: &RunProjection,
    task_samples: &BTreeMap<String, Option<String>>,
) -> Result<(), DbError> {
    if rp.tasks.is_empty() {
        return Ok(());
    }
    let n = rp.tasks.len();
    let mut task_keys: Vec<&str> = Vec::with_capacity(n);
    let mut states: Vec<&str> = Vec::with_capacity(n);
    let mut attempts: Vec<i32> = Vec::with_capacity(n);
    let mut exit_codes: Vec<Option<i32>> = Vec::with_capacity(n);
    let mut batch_job_ids: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut log_streams: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut categories: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut instance_ids: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut started: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut finished: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut cached: Vec<bool> = Vec::with_capacity(n);
    let mut groups: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut images: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut verdict_reasons: Vec<Option<&str>> = Vec::with_capacity(n);
    let mut samples: Vec<Option<&str>> = Vec::with_capacity(n);
    // `deps` is a text[] per row, and UNNEST cannot unnest an array of ragged arrays, so each row's
    // deps travel as a JSON array and are turned back into text[] in SQL.
    let mut deps: Vec<serde_json::Value> = Vec::with_capacity(n);
    for (task_key, tp) in &rp.tasks {
        // Per-task display detail comes from the task's "display attempt" (a pure selector).
        let da = tp.display_attempt();
        task_keys.push(task_key);
        // A task the log left `running` on a run that has since terminated is surfaced as FAILED
        // (CANCELLED on a cancelled run) — a task cannot be running once its run is over.
        states.push(rp.effective_task_state(tp).as_str());
        attempts.push(tp.attempts.len() as i32);
        exit_codes.push(da.and_then(|a| a.exit_code));
        batch_job_ids.push(da.map(|a| a.batch_job_id.as_str()));
        log_streams.push(da.and_then(|a| a.log_stream.as_deref()));
        // A task stranded open on an ended run never reported a result; say so, rather than leave
        // its forced FAILED state looking like the task's own failure.
        // An engine verdict that failed the task (its job did not fail) explains it: its category
        // stands in for the one a failed job would carry, and its reason is kept beside it.
        let verdict = tp.verdict();
        categories.push(if rp.is_stranded(tp) {
            Some(RUN_ENDED_CATEGORY)
        } else {
            da.and_then(|a| a.termination_category.as_ref())
                .or_else(|| verdict.and_then(|v| v.category.as_ref()))
                .map(TerminationCategory::as_str)
        });
        verdict_reasons.push(verdict.and_then(|v| v.reason.as_deref()));
        images.push(tp.image());
        instance_ids.push(da.and_then(|a| a.instance_id.as_ref().map(InstanceId::as_str)));
        started.push(da.and_then(|a| a.started_at_micros));
        finished.push(da.and_then(|a| a.finished_at_micros));
        cached.push(tp.cached);
        groups.push(tp.group.as_deref());
        samples.push(task_samples.get(task_key).and_then(|s| s.as_deref()));
        deps.push(serde_json::Value::from(tp.deps.iter().cloned().collect::<Vec<String>>()));
    }
    sqlx::query(
        r#"INSERT INTO task_projection
             (run_id, task_key, state, attempts,
              exit_code, batch_job_id, log_stream, termination_category,
              instance_id, started_at, finished_at, cached, "group", deps, image, verdict_reason,
              sample)
           SELECT $1, t.task_key, t.state, t.attempts, t.exit_code, t.batch_job_id, t.log_stream,
                  t.termination_category, t.instance_id, t.started_at, t.finished_at, t.cached,
                  t."group", ARRAY(SELECT jsonb_array_elements_text(t.deps)), t.image,
                  t.verdict_reason, t.sample
             FROM UNNEST($2::text[], $3::text[], $4::int[], $5::int[], $6::text[], $7::text[],
                         $8::text[], $9::text[], $10::bigint[], $11::bigint[], $12::bool[],
                         $13::text[], $14::jsonb[], $15::text[], $16::text[], $17::text[])
                  AS t(task_key, state, attempts, exit_code, batch_job_id, log_stream,
                       termination_category, instance_id, started_at, finished_at, cached,
                       "group", deps, image, verdict_reason, sample)
           ON CONFLICT (run_id, task_key) DO UPDATE SET
             state = EXCLUDED.state, attempts = EXCLUDED.attempts,
             exit_code = EXCLUDED.exit_code, batch_job_id = EXCLUDED.batch_job_id,
             log_stream = EXCLUDED.log_stream,
             termination_category = EXCLUDED.termination_category,
             instance_id = EXCLUDED.instance_id, started_at = EXCLUDED.started_at,
             finished_at = EXCLUDED.finished_at, cached = EXCLUDED.cached,
             "group" = EXCLUDED."group", deps = EXCLUDED.deps, image = EXCLUDED.image,
             verdict_reason = EXCLUDED.verdict_reason, sample = EXCLUDED.sample"#,
    )
    .bind(run_id)
    .bind(&task_keys)
    .bind(&states)
    .bind(&attempts)
    .bind(&exit_codes)
    .bind(&batch_job_ids)
    .bind(&log_streams)
    .bind(&categories)
    .bind(&instance_ids)
    .bind(&started)
    .bind(&finished)
    .bind(&cached)
    .bind(&groups)
    .bind(&deps)
    .bind(&images)
    .bind(&verdict_reasons)
    .bind(&samples)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Persist the run's output set into `run_output` so the rows match the fold exactly: upsert every
/// output the fold produced (one statement) and delete any row it no longer produces. The fold is a
/// full replay of the run's events, so a set that shrank (a reducer fix, e.g. the empty-URI guard)
/// must shrink the table too — upsert-only would leave the persisted set a superset of the fold.
async fn upsert_outputs(
    conn: &mut PgConnection,
    run_id: &str,
    rp: &RunProjection,
) -> Result<(), DbError> {
    let outputs: Vec<_> = rp.outputs.values().collect();
    let uris: Vec<&str> = outputs.iter().map(|o| o.s3_uri.as_str()).collect();
    sqlx::query(
        "DELETE FROM run_output o WHERE o.run_id = $1 \
           AND NOT EXISTS (SELECT 1 FROM UNNEST($2::text[]) AS u(uri) WHERE u.uri = o.s3_uri)",
    )
    .bind(run_id)
    .bind(&uris)
    .execute(&mut *conn)
    .await?;
    if outputs.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> =
        uris.iter().map(|u| biffix_core::reducer::output_id(run_id, u)).collect();
    let paths: Vec<Option<&str>> = outputs.iter().map(|o| o.path.as_deref()).collect();
    let task_keys: Vec<Option<&str>> = outputs.iter().map(|o| o.task_key.as_deref()).collect();
    let attempts: Vec<Option<i32>> =
        outputs.iter().map(|o| o.attempt.map(|a| a.0 as i32)).collect();
    let roles: Vec<&str> = outputs.iter().map(|o| o.role.as_str()).collect();
    let reused: Vec<bool> = outputs.iter().map(|o| o.reused).collect();
    let sizes: Vec<Option<i64>> = outputs.iter().map(|o| o.size.map(|s| s as i64)).collect();
    let etags: Vec<Option<&str>> = outputs.iter().map(|o| o.etag.as_deref()).collect();
    sqlx::query(
        r"INSERT INTO run_output
             (run_id, s3_uri, id, path, task_key, attempt, role, reused, size, etag)
           SELECT $1, * FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::int[],
                                    $7::text[], $8::bool[], $9::bigint[], $10::text[])
           ON CONFLICT (run_id, s3_uri) DO UPDATE SET
             id = EXCLUDED.id,
             path = EXCLUDED.path, task_key = EXCLUDED.task_key, attempt = EXCLUDED.attempt,
             role = EXCLUDED.role, reused = EXCLUDED.reused, size = EXCLUDED.size,
             etag = EXCLUDED.etag",
    )
    .bind(run_id)
    .bind(&uris)
    .bind(&ids)
    .bind(&paths)
    .bind(&task_keys)
    .bind(&attempts)
    .bind(&roles)
    .bind(&reused)
    .bind(&sizes)
    .bind(&etags)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The `task_projection.termination_category` of a task stranded open on an ended run (see
/// [`RunProjection::is_stranded`]). A display-only category: no Batch attempt reports it, so
/// `attempt_projection` never carries it.
pub const RUN_ENDED_CATEGORY: &str = "run_ended";

/// Persist every Batch attempt of the run's tasks into `attempt_projection`, in one statement.
///
/// Attempt state is the attempt's own last observed state — unlike `task_projection.state`, it is
/// not coerced to failed when the run has ended, so a reader sees exactly what Batch last reported.
/// The rows match the fold exactly: every attempt the fold produced is upserted and any row it no
/// longer produces is deleted (including when the fold has no attempts at all), so a replay after a
/// reducer fix leaves no stale attempt behind.
///
/// The table holds one row per `(run_id, batch_job_id)`, but the fold records every claim of a job,
/// so a run that claims one job under two of its tasks has it in both (see [`attempt_rows`]).
async fn upsert_attempts(
    conn: &mut PgConnection,
    run_id: &str,
    rp: &RunProjection,
) -> Result<(), DbError> {
    let attempts = attempt_rows(rp);
    let batch_job_ids: Vec<&str> = attempts.iter().map(|(_, a)| a.batch_job_id.as_str()).collect();
    sqlx::query(
        "DELETE FROM attempt_projection a WHERE a.run_id = $1 \
           AND NOT EXISTS (SELECT 1 FROM UNNEST($2::text[]) AS j(id) WHERE j.id = a.batch_job_id)",
    )
    .bind(run_id)
    .bind(&batch_job_ids)
    .execute(&mut *conn)
    .await?;
    if attempts.is_empty() {
        return Ok(());
    }
    let column = |f: fn(&AttemptProjection) -> Option<&str>| -> Vec<Option<&str>> {
        attempts.iter().map(|(_, a)| f(a)).collect()
    };
    let task_keys: Vec<&str> = attempts.iter().map(|(k, _)| *k).collect();
    let attempt_numbers: Vec<Option<i32>> =
        attempts.iter().map(|(_, a)| a.attempt.map(|n| n.0 as i32)).collect();
    let states: Vec<&str> = attempts.iter().map(|(_, a)| a.state.as_str()).collect();
    let exit_codes: Vec<Option<i32>> = attempts.iter().map(|(_, a)| a.exit_code).collect();
    let started: Vec<Option<i64>> = attempts.iter().map(|(_, a)| a.started_at_micros).collect();
    let finished: Vec<Option<i64>> = attempts.iter().map(|(_, a)| a.finished_at_micros).collect();
    let requested_vcpus: Vec<Option<f64>> =
        attempts.iter().map(|(_, a)| a.requested_vcpus).collect();
    let requested_memory: Vec<Option<i64>> =
        attempts.iter().map(|(_, a)| a.requested_memory_mib).collect();
    sqlx::query(
        r"INSERT INTO attempt_projection
             (run_id, batch_job_id, task_key, attempt, state, exit_code, termination_category,
              status_reason, log_stream, queue, instance_id, started_at, finished_at,
              requested_vcpus, requested_memory_mib)
           SELECT $1, * FROM UNNEST($2::text[], $3::text[], $4::int[], $5::text[], $6::int[],
                                    $7::text[], $8::text[], $9::text[], $10::text[], $11::text[],
                                    $12::bigint[], $13::bigint[], $14::float8[], $15::bigint[])
           ON CONFLICT (run_id, batch_job_id) DO UPDATE SET
             task_key = EXCLUDED.task_key, attempt = EXCLUDED.attempt, state = EXCLUDED.state,
             exit_code = EXCLUDED.exit_code,
             termination_category = EXCLUDED.termination_category,
             status_reason = EXCLUDED.status_reason, log_stream = EXCLUDED.log_stream,
             queue = EXCLUDED.queue, instance_id = EXCLUDED.instance_id,
             started_at = EXCLUDED.started_at, finished_at = EXCLUDED.finished_at,
             requested_vcpus = EXCLUDED.requested_vcpus,
             requested_memory_mib = EXCLUDED.requested_memory_mib",
    )
    .bind(run_id)
    .bind(&batch_job_ids)
    .bind(&task_keys)
    .bind(&attempt_numbers)
    .bind(&states)
    .bind(&exit_codes)
    .bind(column(|a| a.termination_category.as_ref().map(TerminationCategory::as_str)))
    .bind(column(|a| a.status_reason.as_deref()))
    .bind(column(|a| a.log_stream.as_deref()))
    .bind(column(|a| a.queue.as_deref()))
    .bind(column(|a| a.instance_id.as_ref().map(InstanceId::as_str)))
    .bind(&started)
    .bind(&finished)
    .bind(&requested_vcpus)
    .bind(&requested_memory)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The run's `attempt_projection` rows as `(task_key, attempt)`, one per Batch job. A job the run
/// claimed under more than one task (a producer bug or a forged claim) is in each claiming task,
/// but only the first claim in the log binds it, so only that task's attempt receives the job's
/// Batch facts; that attempt is the row. Before any fact arrives every claim is a bare `submitted`
/// shell and the first task key in order stands in. One `INSERT … ON CONFLICT DO UPDATE` cannot
/// carry two rows for one key (SQLSTATE 21000), which would refuse the run's whole batch.
fn attempt_rows(rp: &RunProjection) -> Vec<(&str, &AttemptProjection)> {
    fn has_batch_facts(a: &AttemptProjection) -> bool {
        a.state != AttemptState::Submitted
            || a.queue.is_some()
            || a.instance_id.is_some()
            || a.log_stream.is_some()
    }
    let mut by_job: BTreeMap<&str, (&str, &AttemptProjection)> = BTreeMap::new();
    for (task_key, tp) in &rp.tasks {
        for attempt in tp.attempts.values() {
            by_job
                .entry(attempt.batch_job_id.as_str())
                .and_modify(|kept| {
                    if !has_batch_facts(kept.1) && has_batch_facts(attempt) {
                        *kept = (task_key, attempt);
                    }
                })
                .or_insert((task_key, attempt));
        }
    }
    by_job.into_values().collect()
}

/// Re-fold one run from its complete event set and persist it: its own events plus the AWS events
/// for the jobs it bound (which carry a batch_job_id but no run_id). Folding this yields exactly
/// this one run. The caller holds the advisory lock and owns the transaction.
///
/// The two halves are separate `UNION ALL` arms, not one `run_id = $1 OR batch_job_id IN (…)`
/// predicate: Postgres cannot serve an `OR` with a hashed-subquery arm from an index, so that form
/// is a sequential scan of the whole log per run. As arms, the first reads `run_events_run_seq` and
/// the second probes `run_events_batch_lifecycle` once per bound job. The second arm skips rows
/// already in the first (`run_id IS DISTINCT FROM $1`), so no event is read twice.
pub(super) async fn fold_run(conn: &mut PgConnection, run_id: &str) -> Result<(), DbError> {
    let rows = sqlx::query(
        r"SELECT seq, payload FROM (
               SELECT seq, ts_micros, payload FROM run_events WHERE run_id = $1
               UNION ALL
               SELECT e.seq, e.ts_micros, e.payload FROM run_events e
                WHERE e.batch_job_id IN (
                          SELECT b.batch_job_id FROM run_events b
                           WHERE b.run_id = $1 AND b.batch_job_id IS NOT NULL
                      )
                  AND e.run_id IS DISTINCT FROM $1
           ) ev
           ORDER BY ts_micros, seq",
    )
    .bind(run_id)
    .fetch_all(&mut *conn)
    .await?;
    let events = rows_to_events(&rows);
    let projection = Projection::replay(&events);
    match projection.runs.get(run_id) {
        Some(rp) => upsert_run(&mut *conn, run_id, rp).await,
        None => delete_run(conn, run_id).await,
    }
}

/// Drop every row the run fold wrote for `run_id`, now that its events fold to no run (a reducer fix
/// stopped trusting their producer, say): rather than leave an older fold's result behind
/// (`projection == fold`), as `fold_build_subject` does for a build. The schema has no foreign keys,
/// so each table the fold owns is cleared; user state (`notification_reads`,
/// `notification_dismissals`) and the fact caches are not the fold's and stay.
async fn delete_run(conn: &mut PgConnection, run_id: &str) -> Result<(), DbError> {
    for table in
        ["notifications", "run_output", "attempt_projection", "task_projection", "run_projection"]
    {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE run_id = $1")))
            .bind(run_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}
