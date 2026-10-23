//! The retention step: delete the intermediate files (Nextflow `work/`) of runs whose retention
//! says so.

use biffix_core::clock::{micros, since_epoch};
use lambda_runtime::Error;

use super::{aws_error, delete_keys, enqueue_events, note, object_ids, State};
use crate::budget::{fair_share, Deadline};

/// The `reconcile_cursor` name of the retention sweep's place in its rotation.
const RETENTION_CURSOR: &str = "retention";

/// Delete the intermediate files (Nextflow `work/`) of runs whose retention says so, a bounded
/// number of `DeleteObjects` batches per tick shared fairly across the due work dirs
/// ([`fair_share`]) and made only while the invocation has time left; a work dir found empty has
/// its deletion recorded (`work_deleted`) for every run that shares it. A failing work dir is
/// logged and the rest are still swept. Returns how many objects were deleted.
pub(super) async fn sweep_retention(state: &State, deadline: Deadline) -> Result<usize, Error> {
    use crate::retention::{
        cutoff_micros, work_location, DELETE_BATCHES_PER_TICK, GROUPS_PER_TICK,
    };
    use crate::store::cursor::{reconcile_cursor, set_reconcile_cursor};
    use crate::store::retention::work_due_for_deletion;
    let now = since_epoch();
    let cutoff = cutoff_micros(micros(now), state.config.retention_days);
    // This tick's page of due work dirs: the ones after where the last tick stopped, wrapping to
    // the first once it reaches the end, so every due work dir is reached in turn.
    let after = reconcile_cursor(&state.pool, RETENTION_CURSOR).await?;
    let mut due =
        work_due_for_deletion(&state.pool, cutoff, GROUPS_PER_TICK, after.as_deref()).await?;
    if after.is_some() && (due.len() as i64) < GROUPS_PER_TICK {
        let wrapped =
            work_due_for_deletion(&state.pool, cutoff, GROUPS_PER_TICK - due.len() as i64, None)
                .await?;
        let paged: std::collections::BTreeSet<String> =
            due.iter().map(|g| g.output_prefix.clone()).collect();
        due.extend(wrapped.into_iter().filter(|g| !paged.contains(&g.output_prefix)));
    }
    let resume = due.last().map(|g| g.output_prefix.as_str());
    set_reconcile_cursor(&state.pool, RETENTION_CURSOR, resume).await?;
    let ts = biffix_core::time::Timestamp::from_epoch_seconds(now.as_secs_f64());
    let mut deleted = 0usize;
    let mut remaining = DELETE_BATCHES_PER_TICK;
    let mut first_error: Option<Error> = None;
    for (index, group) in due.iter().enumerate() {
        let Some((bucket, prefix)) = work_location(&group.output_prefix) else {
            // The query admits only the exact per-run shape, so this is a bug, not a custom prefix.
            tracing::error!(prefix = %group.output_prefix, "retention: due work dir has no sweepable location");
            continue;
        };
        let budget = fair_share(remaining, due.len() - index);
        match sweep_work_dir(state, group, &bucket, &prefix, cutoff, budget, deadline).await {
            Ok(swept) => {
                remaining = remaining.saturating_sub(swept.batches);
                deleted += swept.deleted;
                if !swept.emptied {
                    continue;
                }
                let events: Vec<_> = group
                    .run_ids
                    .iter()
                    .map(|id| {
                        biffix_core::event::work_deleted(
                            &biffix_core::ids::RunId::new(id.clone()),
                            ts,
                        )
                    })
                    .collect();
                match enqueue_events(state, &events).await {
                    Ok(_) => {
                        tracing::info!(prefix = %group.output_prefix, runs = events.len(), "retention: work dir deleted");
                    }
                    Err(error) => note(&mut first_error, error),
                }
            }
            Err(error) => {
                tracing::warn!(prefix = %group.output_prefix, error = %error, "retention: work dir sweep failed (continuing)");
                note(&mut first_error, error);
            }
        }
    }
    first_error.map_or(Ok(deleted), Err)
}

/// What one tick did to one work dir.
#[derive(Default)]
struct WorkDirSweep {
    deleted: usize,
    batches: usize,
    /// The work dir was found empty, so its deletion can be recorded.
    emptied: bool,
}

/// Delete up to `budget` batches of `s3://bucket/prefix` while the invocation has time left,
/// re-checking before each batch that the work dir is still due (a run that resumed it since the
/// due list was read must keep its `work/`).
async fn sweep_work_dir(
    state: &State,
    group: &crate::retention::WorkDue,
    bucket: &str,
    prefix: &str,
    cutoff: Option<i64>,
    budget: usize,
    deadline: Deadline,
) -> Result<WorkDirSweep, Error> {
    let mut swept = WorkDirSweep::default();
    while deadline.allows_another(since_epoch()) {
        let listed = state
            .s3
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .max_keys(1000)
            .send()
            .await
            .map_err(|error| aws_error(&format!("ListObjectsV2 s3://{bucket}/{prefix}"), error))?;
        let keys = object_ids(listed.contents().iter());
        if keys.is_empty() {
            swept.emptied = true;
            return Ok(swept);
        }
        if swept.batches >= budget {
            return Ok(swept);
        }
        if !crate::store::retention::work_still_due(&state.pool, &group.output_prefix, cutoff)
            .await?
        {
            tracing::info!(prefix = %group.output_prefix, "retention: work dir is no longer due; keeping it");
            return Ok(swept);
        }
        swept.deleted += delete_keys(state, bucket, keys).await?;
        swept.batches += 1;
    }
    Ok(swept)
}
