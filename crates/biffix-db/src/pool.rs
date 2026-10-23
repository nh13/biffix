//! The connection pool every biffix process opens on the one Aurora cluster.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Pool tuning for Aurora Serverless v2 scale-to-zero: wait out the ~15s resume rather than
/// erroring the first requests (`acquire_timeout`), release idle connections so the cluster can
/// pause (`idle_timeout`), recycle long-lived ones (`max_lifetime`), and validate a connection
/// before handing it out so one dropped while paused is discarded, not returned
/// (`test_before_acquire`). Callers pair it with `connect_lazy` so startup never blocks on a live
/// connection. The ONE definition: the backend and the ingest Lambdas connect to the same cluster.
///
/// `idle_timeout` is short (60 s) so a warm API Lambda drops its connections soon after a burst of
/// requests, well inside the cluster's auto-pause delay. It only helps a process that keeps
/// running: the pool's idle reaper is a task, and a Lambda container is FROZEN between
/// invocations, so the reaper never runs there and an idle connection stays open server-side for
/// the container's whole life. A Lambda invoked on a schedule (or by a queue that goes quiet)
/// would so hold the cluster awake forever; it calls [`release_idle`] at the end of every
/// invocation instead. The API Lambda keeps its connections between requests (reconnecting per
/// request would add latency to every UI call) and lets them go once its container is reclaimed.
pub fn tuned_pool_options(max_connections: u32) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(60))
        .max_lifetime(Duration::from_secs(1800))
        .test_before_acquire(true)
}

/// How long [`release_idle`] waits for connections still on their way back to the pool.
const RELEASE_WAIT: Duration = Duration::from_secs(2);

/// Close every connection of `pool`, and return how many were closed. A Lambda calls it as the
/// last step of each invocation, once its work is done, so no connection survives into the frozen
/// period between invocations (see [`tuned_pool_options`]). The pool stays usable: the next query
/// opens a connection again (`min_connections` is 0, so nothing reopens one before that).
///
/// sqlx has no "close the idle connections" call, and `PgPool::close` ends the pool for good, so
/// each idle connection is taken out (`try_acquire` never waits and never opens one) and closed,
/// which also returns its slot to the pool. A connection dropped by the work just done is returned
/// to the pool by a task sqlx spawns, so it may not be idle yet: this waits (at most
/// [`RELEASE_WAIT`]) for the pool to hold none, and warns if one is still checked out then.
pub async fn release_idle(pool: &PgPool) -> usize {
    let started = std::time::Instant::now();
    let mut closed = 0;
    loop {
        while let Some(connection) = pool.try_acquire() {
            if let Err(error) = connection.close().await {
                tracing::debug!(%error, "closing an idle connection failed (it is gone either way)");
            }
            closed += 1;
        }
        if pool.size() == 0 {
            return closed;
        }
        if started.elapsed() >= RELEASE_WAIT {
            tracing::warn!(
                open = pool.size(),
                "a database connection is still checked out at the end of the invocation"
            );
            return closed;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
