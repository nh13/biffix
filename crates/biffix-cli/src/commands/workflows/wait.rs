//! `register --wait`: poll a just-registered version until its engine runtime and images are
//! built and it can run.

use biffix_api::VersionRuntime;

use crate::api::ApiClient;
use crate::error::{CliError, CliResult, TRANSIENT_BUDGET};

/// Where a version's engine runtime is, for `register --wait`.
#[derive(Debug, PartialEq)]
enum RuntimeWait {
    /// A run can launch now.
    Ready,
    /// The runtime failed to build: why, and where its log is.
    Failed(String),
    /// Still building (the state to report).
    Pending(String),
}

/// Where a version is, for `register --wait`: its `state` (runtime and images). A version whose
/// readiness the server could not read is still pending.
fn version_wait(detail: &biffix_api::WorkflowVersionDetail) -> RuntimeWait {
    let reasons = || detail.not_ready.join("; ");
    match detail.state.as_deref() {
        Some("ready") => RuntimeWait::Ready,
        Some("failed") if detail.not_ready.is_empty() => RuntimeWait::Failed(format!(
            "the version cannot run: its runtime or an image failed to build (see `biffix \
             workflows show {}`)",
            detail.id
        )),
        Some("failed") => RuntimeWait::Failed(reasons()),
        Some(_) if !detail.not_ready.is_empty() => RuntimeWait::Pending(reasons()),
        Some(state) => RuntimeWait::Pending(state.to_string()),
        None => RuntimeWait::Pending("its readiness could not be read".into()),
    }
}

/// How often [`wait_for_runtime`] polls.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// Poll the version until its runtime is ready, failed, or `timeout` passes. A blip while polling
/// (the service unavailable, a 5xx) is polled through within [`TRANSIENT_BUDGET`], as `runs
/// follow` does: the registration already succeeded. Every error names the version, so a script
/// can pick it up again.
pub(super) async fn wait_for_runtime(
    client: &ApiClient,
    id: &str,
    timeout: std::time::Duration,
) -> CliResult<Option<VersionRuntime>> {
    // A timeout too large to add to now is no deadline at all.
    let deadline = std::time::Instant::now().checked_add(timeout);
    let mut last = String::new();
    let mut transient = 0u32;
    loop {
        match client.get_workflow(id).await {
            Ok(detail) => {
                transient = 0;
                match version_wait(&detail) {
                    RuntimeWait::Ready => return Ok(detail.runtime),
                    RuntimeWait::Failed(why) => {
                        return Err(CliError::Unclassified(format!("{id}: {why}")))
                    }
                    RuntimeWait::Pending(state) => {
                        if state != last {
                            eprintln!("[biffix] waiting: {state}…");
                            last = state;
                        }
                    }
                }
            }
            Err(e) if e.is_transient() && transient < TRANSIENT_BUDGET => transient += 1,
            Err(e) => return Err(e.map_message(|m| format!("{id}: {m}"))),
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return Err(CliError::Unavailable(format!(
                "{id} is not ready after {} min; its runtime or images are still being built \
                 (check `biffix workflows show {id}`, or pass --no-wait not to wait)",
                timeout.as_secs() / 60
            )));
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(state: Option<&str>, not_ready: &[&str]) -> biffix_api::WorkflowVersionDetail {
        let mut json = serde_json::json!({
            "id": "a:h:0", "name": "wf", "project": "p", "image_digest": "",
            "engine": "snakemake", "engine_version": "9.27.0", "git_sha": "a",
            "queue_classes": [], "created_at_micros": 0, "source_content_hash": "h",
            "entrypoint": "Snakefile", "form_schema": {"fields": [], "warnings": []},
            "source": {"kind": "git", "repo": "r", "git_ref": "main"}, "build": 0,
            "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
            "not_ready": not_ready,
        });
        if let Some(state) = state {
            json["state"] = state.into();
        }
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn waiting_reads_the_version_s_state_before_its_runtime() {
        assert_eq!(version_wait(&detail(Some("ready"), &[])), RuntimeWait::Ready);
        assert_eq!(
            version_wait(&detail(Some("building"), &["runtime: building", "image x: queued"])),
            RuntimeWait::Pending("runtime: building; image x: queued".into())
        );
        assert_eq!(
            version_wait(&detail(Some("building"), &[])),
            RuntimeWait::Pending("building".into())
        );
        assert_eq!(
            version_wait(&detail(Some("failed"), &["image x: FAILED"])),
            RuntimeWait::Failed("image x: FAILED".into())
        );
        // A failure with no reason still says what happened and where to look.
        let RuntimeWait::Failed(why) = version_wait(&detail(Some("failed"), &[])) else {
            panic!("a failed version ends the wait");
        };
        assert!(why.contains("failed to build") && why.contains("workflows show a:h:0"), "{why}");
        // A readiness the server could not read: keep waiting.
        assert_eq!(
            version_wait(&detail(None, &[])),
            RuntimeWait::Pending("its readiness could not be read".into())
        );
    }
}
