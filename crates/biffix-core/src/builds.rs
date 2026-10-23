//! The build projection: each build subject's latest build, as a fold of its `build_*` events.
//!
//! A subject (`runtime:<key>`, `image:<version>/<name>`, …) names something biffix builds with
//! CodeBuild. It can be built more than once (a failed build retried, a rebuild), and its latest
//! build — the one first seen last — is the one that counts. Within one build the state only moves
//! forward (requested → running → succeeded | failed), whatever order its events arrive in: two
//! state changes in the same second can be delivered out of order.
//!
//! Like the run projection, `projection == fold(events)`: a bug here is fixed in this fold and the
//! table re-derived, never by patching rows.

use std::collections::BTreeMap;

use crate::event::{EventData, RunEvent};
use crate::state::UnknownState;
use crate::time::Timestamp;

/// Where a subject's latest build is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuildState {
    Requested,
    Running,
    Succeeded,
    Failed,
}

impl BuildState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

impl std::fmt::Display for BuildState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The inverse of [`BuildState::as_str`].
impl std::str::FromStr for BuildState {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "requested" => Ok(Self::Requested),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            other => Err(UnknownState(other.to_string())),
        }
    }
}

/// One subject's projection.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildProjection {
    pub subject: String,
    pub state: BuildState,
    /// The latest build.
    pub build_arn: String,
    /// What the latest build produced (its exported variables), once it succeeded.
    pub outputs: BTreeMap<String, String>,
    /// Why the latest build failed.
    pub reason: Option<String>,
    pub logs_url: Option<String>,
    /// What biffix asked the latest build for (from its `build_requested`), or null.
    pub request: serde_json::Value,
    /// How many builds of the subject have been seen.
    pub builds: u32,
    /// The newest event's time.
    pub updated: Timestamp,
}

/// The subject a build event is about, or `None` for any other event.
pub fn subject_of(event: &RunEvent) -> Option<&str> {
    match &event.data {
        EventData::BuildRequested { subject, .. }
        | EventData::BuildStarted { subject, .. }
        | EventData::BuildSucceeded { subject, .. }
        | EventData::BuildFailed { subject, .. } => Some(subject),
        _ => None,
    }
}

/// One build of a subject, as folded so far.
struct Build {
    /// Its first event's position in log order (`ts`, then `seq`): orders the subject's builds.
    first_seen: usize,
    state: BuildState,
    outputs: BTreeMap<String, String>,
    reason: Option<String>,
    logs_url: Option<String>,
    request: serde_json::Value,
}

/// Fold the build events in `events` into one projection per subject. Events that are not about a
/// build are ignored.
///
/// Like [`crate::reducer::Projection::replay`], the fold puts the events in log order itself —
/// source time, then log sequence ([`crate::event::LogSeq`]) — so which build is a subject's latest
/// is intrinsic to the events, not to the order the caller passed them. Events with no `seq` sort
/// before sequenced ones at the same `ts` and otherwise keep their input order (the sort is stable).
pub fn fold_builds(events: &[RunEvent]) -> BTreeMap<String, BuildProjection> {
    let mut ordered: Vec<&RunEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.log_order());
    let mut subjects: BTreeMap<String, (BTreeMap<String, Build>, Timestamp)> = BTreeMap::new();
    for (position, event) in ordered.into_iter().enumerate() {
        // A build fact only its producer may write (`build_requested` is the control plane's)
        // folds only from that producer, as in the run fold: a row planted under the kind from
        // any other source is kept by the log but moves nothing.
        if !crate::reducer::is_trusted_producer(event) {
            continue;
        }
        let (subject, build_arn, state) = match &event.data {
            EventData::BuildRequested { subject, build_arn, .. } => {
                (subject, build_arn, BuildState::Requested)
            }
            EventData::BuildStarted { subject, build_arn, .. } => {
                (subject, build_arn, BuildState::Running)
            }
            EventData::BuildSucceeded { subject, build_arn, .. } => {
                (subject, build_arn, BuildState::Succeeded)
            }
            EventData::BuildFailed { subject, build_arn, .. } => {
                (subject, build_arn, BuildState::Failed)
            }
            _ => continue,
        };
        let (builds, updated) =
            subjects.entry(subject.clone()).or_insert_with(|| (BTreeMap::new(), event.ts));
        *updated = (*updated).max(event.ts);
        let build = builds.entry(build_arn.clone()).or_insert_with(|| Build {
            first_seen: position,
            state,
            outputs: BTreeMap::new(),
            reason: None,
            logs_url: None,
            request: serde_json::Value::Null,
        });
        // Forward only: a late `started` never reopens a finished build.
        if !build.state.is_terminal() && state > build.state {
            build.state = state;
        }
        match &event.data {
            EventData::BuildRequested { request, .. } => build.request = request.clone(),
            EventData::BuildStarted { logs_url, .. } => {
                build.logs_url = build.logs_url.take().or_else(|| logs_url.clone());
            }
            EventData::BuildSucceeded { outputs, logs_url, .. } => {
                build.outputs = outputs.clone();
                build.logs_url = logs_url.clone().or(build.logs_url.take());
            }
            EventData::BuildFailed { reason, logs_url, .. } => {
                build.reason = Some(reason.clone());
                build.logs_url = logs_url.clone().or(build.logs_url.take());
            }
            _ => {}
        }
    }
    subjects
        .into_iter()
        // A subject entry is only ever created together with its first build, so the latest
        // always exists; `filter_map` just keeps that invariant from needing a panic.
        .filter_map(|(subject, (builds, updated))| {
            let count = builds.len() as u32;
            let (build_arn, latest) =
                builds.into_iter().max_by_key(|(_, build)| build.first_seen)?;
            let projection = BuildProjection {
                subject: subject.clone(),
                state: latest.state,
                build_arn,
                outputs: latest.outputs,
                reason: latest.reason,
                logs_url: latest.logs_url,
                request: latest.request,
                builds: count,
                updated,
            };
            Some((subject, projection))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::build_requested;
    use crate::translate::from_aws_codebuild;
    use crate::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};

    fn codebuild(secs: f64, arn: &str, status: CodeBuildStatus, subject: &str) -> RunEvent {
        let succeeded = status == CodeBuildStatus::Succeeded;
        let failed = status == CodeBuildStatus::Failed;
        from_aws_codebuild(&CodeBuildEvent {
            ts: Timestamp::from_epoch_seconds(secs),
            build_arn: arn.into(),
            project: "biffix-t-runtime-build".into(),
            status,
            subject: subject.into(),
            outputs: if succeeded {
                BTreeMap::from([("DIGEST".into(), format!("sha256:{arn}"))])
            } else {
                BTreeMap::new()
            },
            reason: failed.then(|| "BUILD: exit status 3".into()),
            logs_url: Some(format!("https://logs/{arn}")),
        })
    }

    fn requested(secs: f64, arn: &str, subject: &str) -> RunEvent {
        build_requested(
            Timestamp::from_epoch_seconds(secs),
            subject,
            arn,
            serde_json::json!({"engine": "snakemake", "version": "9.3.0"}),
        )
    }

    #[test]
    fn a_build_moves_from_requested_to_its_outputs() {
        let events = [
            requested(1.0, "b1", "runtime:k"),
            codebuild(2.0, "b1", CodeBuildStatus::InProgress, "runtime:k"),
            codebuild(9.0, "b1", CodeBuildStatus::Succeeded, "runtime:k"),
        ];
        let got = &fold_builds(&events)["runtime:k"];
        assert_eq!(got.state, BuildState::Succeeded);
        assert_eq!(got.outputs["DIGEST"], "sha256:b1");
        assert_eq!(got.request["version"], "9.3.0");
        assert_eq!(got.logs_url.as_deref(), Some("https://logs/b1"));
        assert_eq!((got.builds, got.updated), (1, Timestamp::from_epoch_seconds(9.0)));
    }

    #[test]
    fn a_state_change_delivered_late_never_reopens_a_finished_build() {
        // Both in the same second, the terminal logged first.
        let events = [
            codebuild(5.0, "b1", CodeBuildStatus::Succeeded, "image:v/default"),
            codebuild(5.0, "b1", CodeBuildStatus::InProgress, "image:v/default"),
        ];
        assert_eq!(fold_builds(&events)["image:v/default"].state, BuildState::Succeeded);
    }

    #[test]
    fn a_failure_carries_its_reason() {
        let events = [codebuild(3.0, "b1", CodeBuildStatus::Failed, "runtime:k")];
        let got = &fold_builds(&events)["runtime:k"];
        assert_eq!(got.state, BuildState::Failed);
        assert_eq!(got.reason.as_deref(), Some("FAILED: BUILD: exit status 3"));
    }

    #[test]
    fn the_latest_build_counts_and_an_older_build_s_late_event_does_not_override_it() {
        let events = [
            codebuild(1.0, "b1", CodeBuildStatus::InProgress, "runtime:k"),
            requested(4.0, "b2", "runtime:k"),
            // b1 finishes after b2 was requested.
            codebuild(6.0, "b1", CodeBuildStatus::Failed, "runtime:k"),
            codebuild(7.0, "b2", CodeBuildStatus::InProgress, "runtime:k"),
        ];
        let got = &fold_builds(&events)["runtime:k"];
        assert_eq!(
            (got.build_arn.as_str(), got.state, got.builds, got.reason.clone()),
            ("b2", BuildState::Running, 2, None)
        );
    }

    #[test]
    fn a_build_request_from_anyone_but_the_control_plane_moves_nothing() {
        // A `build_requested` row an engine (say) got into the log, e.g. stored as an unknown
        // kind by an older build, neither creates a build nor sets a real build's request.
        let mut planted = requested(1.0, "b9", "runtime:k");
        planted.source = crate::source::EventSource::Nextflow;
        let mut planted_on_real = requested(3.0, "b1", "runtime:k");
        planted_on_real.source = crate::source::EventSource::Snakemake;
        if let EventData::BuildRequested { request, .. } = &mut planted_on_real.data {
            *request = serde_json::json!({"forged": true});
        }
        let events = [
            planted,
            requested(2.0, "b1", "runtime:k"),
            planted_on_real,
            codebuild(4.0, "b1", CodeBuildStatus::InProgress, "runtime:k"),
        ];
        let got = &fold_builds(&events)["runtime:k"];
        assert_eq!((got.build_arn.as_str(), got.state, got.builds), ("b1", BuildState::Running, 1));
        assert_eq!(got.request["version"], "9.3.0");
        assert_eq!(got.updated, Timestamp::from_epoch_seconds(4.0));

        let only_planted = {
            let mut e = requested(1.0, "b9", "image:v/x");
            e.source = crate::source::EventSource::Nextflow;
            [e]
        };
        assert!(fold_builds(&only_planted).is_empty());
    }

    #[test]
    fn build_state_parses_as_the_inverse_of_as_str() {
        for state in
            [BuildState::Requested, BuildState::Running, BuildState::Succeeded, BuildState::Failed]
        {
            assert_eq!(state.as_str().parse::<BuildState>(), Ok(state));
        }
        assert_eq!("nope".parse::<BuildState>(), Err(UnknownState("nope".into())));
    }

    #[test]
    fn subjects_fold_independently_and_other_events_are_ignored() {
        let events = [
            requested(1.0, "b1", "runtime:a"),
            requested(1.0, "b2", "image:v/x"),
            codebuild(2.0, "b2", CodeBuildStatus::Succeeded, "image:v/x"),
        ];
        let folded = fold_builds(&events);
        assert_eq!(folded.len(), 2);
        assert_eq!(folded["runtime:a"].state, BuildState::Requested);
        assert_eq!(folded["image:v/x"].state, BuildState::Succeeded);
        assert_eq!(subject_of(&events[1]), Some("image:v/x"));
    }

    /// Stamp `events` with log sequence numbers in the given order, as the log would.
    fn sequenced<const N: usize>(mut events: [RunEvent; N]) -> [RunEvent; N] {
        for (i, event) in events.iter_mut().enumerate() {
            event.seq = Some(crate::event::LogSeq(i as i64 + 1));
        }
        events
    }

    #[test]
    fn equal_time_builds_order_by_log_sequence_not_by_input_order() {
        // Two builds requested in the same control-plane tick, finishing in the same second: only
        // `seq` says b2 is the later build. Passing the slice in either order must agree.
        let events = sequenced([
            requested(4.0, "b1", "runtime:k"),
            requested(4.0, "b2", "runtime:k"),
            codebuild(5.0, "b1", CodeBuildStatus::Failed, "runtime:k"),
            codebuild(5.0, "b2", CodeBuildStatus::Succeeded, "runtime:k"),
        ]);
        let mut reversed = events.clone();
        reversed.reverse();
        for slice in [&events[..], &reversed[..]] {
            let got = &fold_builds(slice)["runtime:k"];
            assert_eq!(
                (got.build_arn.as_str(), got.state, got.builds),
                ("b2", BuildState::Succeeded, 2)
            );
        }
    }

    #[test]
    fn the_fold_is_a_function_of_the_event_set_in_any_input_order() {
        let events = sequenced([
            requested(1.0, "b1", "runtime:k"),
            codebuild(2.0, "b1", CodeBuildStatus::InProgress, "runtime:k"),
            codebuild(3.0, "b1", CodeBuildStatus::Failed, "runtime:k"),
            requested(4.0, "b2", "runtime:k"),
            codebuild(5.0, "b2", CodeBuildStatus::Succeeded, "runtime:k"),
        ]);
        let whole = fold_builds(&events);
        // Every rotation and the reversal of the input fold to the same projection.
        for shift in 0..events.len() {
            let mut rotated = events.to_vec();
            rotated.rotate_left(shift);
            assert_eq!(fold_builds(&rotated), whole, "rotation {shift}");
            rotated.reverse();
            assert_eq!(fold_builds(&rotated), whole, "reversed rotation {shift}");
        }
        // Replaying the log — the events as stored (JSON) and read back — gives the same
        // projection as folding them as they were produced.
        let stored: Vec<RunEvent> = events
            .iter()
            .map(|e| serde_json::from_value(serde_json::to_value(e).unwrap()).unwrap())
            .collect();
        let replayed = fold_builds(&stored);
        assert_eq!(whole, replayed);
        assert_eq!(whole["runtime:k"].state, BuildState::Succeeded);
        assert_eq!(whole["runtime:k"].builds, 2);
    }
}
