//! Logging for the deploy's Lambdas (the backend's API and sweep, ingest's drain and reconcile).

/// Install the one log format every biffix Lambda writes: JSON lines at `info` and above, with no
/// timestamp of their own (CloudWatch stamps each line as it receives it).
pub fn init_lambda() {
    tracing_subscriber::fmt().with_env_filter("info").json().without_time().init();
}
