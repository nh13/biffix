//! The cache-expiry step: delete expired Snakemake between-workflow cache entries.

use biffix_core::clock::{epoch_micros, since_epoch};
use lambda_runtime::Error;

use super::{aws_error, delete_keys, note, object_ids, State};
use crate::budget::{fair_share, Deadline};

/// Delete Snakemake cache entries older than the deploy's cache period from each project bucket,
/// examining a bounded number of listing pages per tick (a fair share per bucket, while the
/// invocation has time left) and resuming where the last tick stopped. A failing bucket is logged
/// and the rest are still swept. Returns how many entries were deleted.
pub(super) async fn sweep_snakemake_cache(
    state: &State,
    deadline: Deadline,
) -> Result<usize, Error> {
    use crate::cache_expiry::LIST_PAGES_PER_TICK;
    use crate::retention::cutoff_micros;
    let Some(cutoff) = cutoff_micros(epoch_micros(), state.config.cache_days) else {
        return Ok(0);
    };
    let mut deleted = 0usize;
    let mut remaining = LIST_PAGES_PER_TICK;
    let mut first_error: Option<Error> = None;
    let buckets = &state.config.cache_buckets;
    for (index, bucket) in buckets.iter().enumerate() {
        let pages = fair_share(remaining, buckets.len() - index);
        match sweep_cache_bucket(state, bucket, cutoff, pages, deadline).await {
            Ok(swept) => {
                deleted += swept.deleted;
                remaining = remaining.saturating_sub(swept.pages);
            }
            Err(error) => {
                tracing::warn!(bucket, error = %error, "cache: bucket sweep failed (continuing)");
                note(&mut first_error, error);
            }
        }
    }
    first_error.map_or(Ok(deleted), Err)
}

/// What one tick did to one bucket.
#[derive(Default)]
struct CacheSweep {
    deleted: usize,
    pages: usize,
}

/// One bucket's share of [`sweep_snakemake_cache`]: list up to `pages` pages from the saved cursor
/// (`reconcile_cursor`), delete what expired, and save after every page where to resume — or clear
/// the cursor once a pass reached the end, so the next tick starts over. A failure, or the
/// invocation running out of time, keeps every completed page's progress.
async fn sweep_cache_bucket(
    state: &State,
    bucket: &str,
    cutoff: i64,
    pages: usize,
    deadline: Deadline,
) -> Result<CacheSweep, Error> {
    use crate::cache_expiry::{cursor_name, is_expired, next_cursor};
    use crate::store::cursor::{reconcile_cursor, set_reconcile_cursor};
    use biffix_core::s3_layout::SNAKEMAKE_CACHE_PREFIX;
    let name = cursor_name(bucket);
    let start_after = reconcile_cursor(&state.pool, &name).await?;
    let mut continuation: Option<String> = None;
    let mut swept = CacheSweep::default();
    while swept.pages < pages && deadline.allows_another(since_epoch()) {
        let listed = state
            .s3
            .list_objects_v2()
            .bucket(bucket)
            .prefix(SNAKEMAKE_CACHE_PREFIX)
            // The continuation token carries the position after the first page.
            .set_start_after(if swept.pages == 0 { start_after.clone() } else { None })
            .set_continuation_token(continuation.take())
            .max_keys(1000)
            .send()
            .await
            .map_err(|error| {
                aws_error(&format!("ListObjectsV2 s3://{bucket}/{SNAKEMAKE_CACHE_PREFIX}"), error)
            })?;
        let expired = object_ids(listed.contents().iter().filter(|o| {
            let modified = o
                .last_modified()
                .and_then(|t| t.to_millis().ok())
                .map(|ms| ms.saturating_mul(1000));
            o.key().is_some_and(|k| is_expired(k, modified, cutoff))
        }));
        if !expired.is_empty() {
            swept.deleted += delete_keys(state, bucket, expired).await?;
        }
        swept.pages += 1;
        let token =
            listed.next_continuation_token().filter(|_| listed.is_truncated().unwrap_or(false));
        let last_key = listed.contents().last().and_then(|o| o.key());
        set_reconcile_cursor(&state.pool, &name, next_cursor(last_key, token.is_some()).as_deref())
            .await?;
        match token {
            Some(token) => continuation = Some(token.to_owned()),
            None => break,
        }
    }
    Ok(swept)
}
