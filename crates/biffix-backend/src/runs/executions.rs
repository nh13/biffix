//! How the API words an attempt's executions (each time AWS Batch ran its job on a host): why an
//! execution ended early ([`interruption`]) and whether Batch is still finding a host to run the
//! attempt again on ([`awaiting_host`]). Pure, so the readers that fill them share one rule.

use biffix_api::JobExecution;
use biffix_core::event::TerminationCategory;
use biffix_core::failure::FailureCause;
use biffix_core::state::{AttemptState, TaskState};

/// `JobExecution.interruption` of an execution that ended because its spot host was reclaimed:
/// spelled as the failure summary's cause for the same loss.
pub(crate) const SPOT_RECLAIMED: &str = FailureCause::SpotReclaimed.as_str();
/// `JobExecution.interruption` of an execution that ended because its host went away otherwise.
pub(crate) const HOST_LOST: &str = FailureCause::HostLost.as_str();

/// Why an execution ended early, from its termination category, its host's lifecycle (the host
/// registry's `spot`/`on-demand`, when known) and whether EC2 warned of a spot reclaim while it
/// ran: `None` unless the category says the host went away; then [`SPOT_RECLAIMED`] when anything
/// says the host was spot, else [`HOST_LOST`]. A queue's configured capacity is not evidence: the
/// queue may mix spot and on-demand hosts.
pub(crate) fn interruption(
    category: Option<&str>,
    lifecycle: Option<&str>,
    spot_warned: bool,
) -> Option<&'static str> {
    let category = TerminationCategory::parse(category?);
    if !category.is_host_lost() {
        return None;
    }
    let spot = category == TerminationCategory::Spot || lifecycle == Some("spot") || spot_warned;
    Some(if spot { SPOT_RECLAIMED } else { HOST_LOST })
}

/// Whether AWS Batch is finding a host to run an attempt again on: the attempt (`attempt_state`)
/// and its task (`task_state`) are both still open, every execution so far (`executions`, in
/// order) has ended, and the last one lost its host. An unknown state is not open.
pub(crate) fn awaiting_host(
    attempt_state: &str,
    task_state: &str,
    executions: &[JobExecution],
) -> bool {
    let attempt_open =
        attempt_state.parse::<AttemptState>().is_ok_and(|state| !state.is_terminal());
    let task_open = matches!(task_state.parse(), Ok(TaskState::Queued | TaskState::Running));
    attempt_open
        && task_open
        && executions.iter().all(|x| x.ended)
        && executions.last().is_some_and(|x| x.interruption.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_lost_host_is_an_interruption_and_any_spot_evidence_makes_it_a_reclaim() {
        let cases = [
            // (category, lifecycle, warned) => interruption
            ((None, Some("spot"), true), None),
            ((Some("oom"), Some("spot"), true), None),
            ((Some("app_error"), None, false), None),
            ((Some("something_new"), Some("spot"), true), None),
            ((Some("spot"), None, false), Some(SPOT_RECLAIMED)),
            ((Some("spot"), Some("on-demand"), false), Some(SPOT_RECLAIMED)),
            ((Some("host_terminated"), Some("spot"), false), Some(SPOT_RECLAIMED)),
            ((Some("host_terminated"), None, true), Some(SPOT_RECLAIMED)),
            ((Some("host_terminated"), Some("on-demand"), false), Some(HOST_LOST)),
            ((Some("host_terminated"), None, false), Some(HOST_LOST)),
        ];
        for ((category, lifecycle, warned), want) in cases {
            assert_eq!(
                interruption(category, lifecycle, warned),
                want,
                "{category:?} {lifecycle:?} {warned}"
            );
        }
    }

    fn execution(ended: bool) -> JobExecution {
        JobExecution {
            execution_id: "x".into(),
            ordinal: 1,
            ended,
            final_execution: true,
            exit_code: None,
            termination_category: None,
            status_reason: None,
            log_stream: None,
            instance_id: None,
            started_at: None,
            stopped_at: None,
            host: None,
            spot_warned_at: None,
            interruption: None,
            cost: None,
        }
    }

    #[test]
    fn an_open_attempt_whose_last_execution_lost_its_host_awaits_a_new_host() {
        let lost = JobExecution { interruption: Some(SPOT_RECLAIMED.into()), ..execution(true) };
        let ended = [execution(true), lost.clone()];
        assert!(awaiting_host("running", "running", &ended));
        assert!(awaiting_host("queued", "queued", &ended));
        // The last one still running, or ended without losing its host (its terminal is still
        // to come); none yet; the attempt or its task over; a state this build does not know.
        assert!(!awaiting_host("running", "running", &[lost.clone(), execution(false)]));
        assert!(!awaiting_host("running", "running", &[lost, execution(true)]));
        assert!(!awaiting_host("queued", "queued", &[]));
        assert!(!awaiting_host("failed", "running", &ended));
        assert!(!awaiting_host("succeeded", "succeeded", &ended));
        assert!(!awaiting_host("running", "cancelled", &ended));
        assert!(!awaiting_host("running", "failed", &ended));
        assert!(!awaiting_host("paused", "running", &ended));
    }
}
