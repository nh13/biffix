//! Following a CodeBuild build to its result, shared by every kind of build the backend starts:
//! coordinator variants ([`crate::images::variants`]), image imports ([`crate::images`]), derives
//! ([`crate::images::derived`]) and images built from a recipe ([`crate::images::build`]).
//!
//! A subject's build state is the build projection (the fold of its events). Each kind records
//! its own result (a job definition, an image artifact), and each reads that result from the
//! build's outputs its own way, so each passes in whether a succeeded build reported one. A build
//! that succeeded but reported nothing usable is a failed build: nothing can be recorded from it,
//! so people are told it failed and a rebuild may start another.

use biffix_core::builds::{BuildProjection, BuildState};

use crate::aws::AwsError;
use crate::store::StoreError;
use crate::AppState;

/// How many builds of one subject are started without a person asking: the first, and the
/// retries of a failed one that registration, a submit and the sweep start. A manual rebuild is
/// not limited.
pub const AUTO_BUILDS: u32 = 3;

/// Why a build succeeded yet counts as failed.
pub(crate) const NO_OUTPUT: &str = "the build succeeded but reported no image";

/// Why a build step (an import, a derive, a recipe build, a coordinator variant, or the sweeps that
/// move them) failed: every kind of build shares it.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("aws: {0}")]
    Aws(#[from] AwsError),
    #[error("the version's runtime recipe is unreadable: {0}")]
    Recipe(String),
}

/// Where a build subject (a variant, an import, a derive, a built image) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Never built.
    Unbuilt,
    /// Its build is requested or running, or it is built and its result not yet recorded.
    Building,
    /// Its latest build failed, or succeeded without reporting a result.
    Failed,
    /// Its result is recorded.
    Ready,
}

impl Phase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unbuilt => "unbuilt",
            Self::Building => "building",
            Self::Failed => "failed",
            Self::Ready => "ready",
        }
    }
}

/// The next step for a subject with one build whose result is recorded: nothing to do, start a
/// build, or record what the succeeded build reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FollowStep<R> {
    Nothing,
    StartBuild,
    /// Record the result of the succeeded build.
    Record(R),
}

/// The one retry-and-record rule every kind of build follows. Nothing once the result is
/// `recorded`; else a build starts when there is none, or when the latest failed (or succeeded but
/// reported nothing `extract` can use) and `retry` allows another; a succeeded build's result is
/// recorded; a build in flight is waited for.
pub(crate) fn follow_step<R>(
    recorded: bool,
    build: Option<&BuildProjection>,
    retry: impl Fn(&BuildProjection) -> bool,
    extract: impl FnOnce(&BuildProjection) -> Option<R>,
) -> FollowStep<R> {
    if recorded {
        return FollowStep::Nothing;
    }
    match build {
        None => FollowStep::StartBuild,
        Some(b) => match b.state {
            BuildState::Requested | BuildState::Running => FollowStep::Nothing,
            BuildState::Failed if retry(b) => FollowStep::StartBuild,
            BuildState::Failed => FollowStep::Nothing,
            BuildState::Succeeded => match extract(b) {
                Some(r) => FollowStep::Record(r),
                // A build that reported nothing usable cannot be recorded: build it again.
                None if retry(b) => FollowStep::StartBuild,
                None => FollowStep::Nothing,
            },
        },
    }
}

/// Whether `build` failed: it ended failed, or it succeeded but `reported` says it reported no
/// usable result.
pub(crate) fn has_failed(
    build: &BuildProjection,
    reported: impl FnOnce(&BuildProjection) -> bool,
) -> bool {
    match build.state {
        BuildState::Failed => true,
        BuildState::Succeeded => !reported(build),
        BuildState::Requested | BuildState::Running => false,
    }
}

/// The phase of a subject with one build: ready once its result is `recorded`, else from its
/// latest build (see [`has_failed`]).
pub(crate) fn phase_of_build(
    recorded: bool,
    build: Option<&BuildProjection>,
    reported: impl FnOnce(&BuildProjection) -> bool,
) -> Phase {
    match build {
        _ if recorded => Phase::Ready,
        None => Phase::Unbuilt,
        Some(b) if has_failed(b, reported) => Phase::Failed,
        Some(_) => Phase::Building,
    }
}

/// Why a failed build failed, as its events tell it (or, for a build that succeeded without a
/// result, [`NO_OUTPUT`]).
/// A build's output `key`, when it reported a non-empty one.
pub(crate) fn output<'a>(build: &'a BuildProjection, key: &str) -> Option<&'a str> {
    build.outputs.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

pub(crate) fn failure_reason(build: &BuildProjection) -> Option<String> {
    build
        .reason
        .clone()
        .or_else(|| (build.state == BuildState::Succeeded).then(|| NO_OUTPUT.to_string()))
}

/// The CodeBuild idempotency token for starting a build of `subject` when it has had
/// `builds_seen` builds: sha256-hex of `"{subject}#{builds_seen}"`.
///
/// Every start reads the subject's build, decides to start one, calls CodeBuild, then records
/// `build_requested`; nothing serializes those steps, so two callers (two registrations, or a
/// registration and the sweep) that read the same state would each start a build. They send the
/// same token, so CodeBuild returns the first call's build to the second instead of starting
/// another, and `build_requested` is deduplicated on the build ARN, so the subject's `builds` count
/// grows once. A later deliberate start (a retry, a rebuild) has seen one more build, so it gets a
/// new token.
///
/// CodeBuild honours a token for 5 minutes. That suffices: the race is between callers that read
/// the same state, and the first records `build_requested` right after CodeBuild answers, so a
/// caller reading after that sees the build and does not start one. A start after the window with
/// the same count means the first start's `build_requested` was never recorded; starting again
/// then is the retry the sweep would make anyway.
pub(crate) fn start_token(subject: &str, builds_seen: u32) -> String {
    crate::sha256_hex(format!("{subject}#{builds_seen}"))
}

/// How many builds a subject has had, from its latest build (if any).
pub(crate) fn builds_seen(build: Option<&BuildProjection>) -> u32 {
    build.map_or(0, |b| b.builds)
}

/// The CodeBuild project a build runs in, which bounds how long it can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuildKind {
    /// A coordinator variant (`codebuild/runtime/build.sh`): the runtime project, 30 minutes.
    Runtime,
    /// An import, a derive or a built image's index (`codebuild/image/import.sh`): the import
    /// project, 120 minutes.
    Import,
    /// One architecture's build of an image (`codebuild/image/build.sh`): the per-architecture
    /// build projects, 60 minutes.
    ArchBuild,
}

impl BuildKind {
    /// A build of this kind with no news for this long is asked about directly (its state change
    /// may have been lost): its project's timeout (`infra/biffix_infra/stacks/build_stack.py`),
    /// plus time in CodeBuild's queue.
    pub(crate) fn stale_after_micros(self) -> i64 {
        let timeout_minutes = match self {
            Self::Runtime => 30,
            Self::Import => 120,
            Self::ArchBuild => 60,
        };
        (timeout_minutes + 15) * 60 * 1_000_000
    }
}

/// Record that build `build_arn` of `subject` was started, now, with what it was asked to do
/// (`request`): the `build_requested` every kind of build records once CodeBuild answers.
pub(crate) async fn record_requested(
    state: &AppState,
    subject: &str,
    build_arn: &str,
    request: serde_json::Value,
) -> Result<(), BuildError> {
    let requested = biffix_core::event::build_requested(
        biffix_core::time::Timestamp::from_epoch_seconds(biffix_core::clock::epoch_secs()),
        subject,
        build_arn,
        request,
    );
    state.events.record(&requested).await?;
    Ok(())
}

/// The sweep's backstop for a build whose end never reached the log: when `build` has had no news
/// for its kind's [`BuildKind::stale_after_micros`], CodeBuild is asked directly and its current
/// state appended as the state-change event would have been (same dedup key, so a late real event
/// is a no-op). Returns the subject's build as it now stands.
pub(crate) async fn reconcile_stale_build(
    state: &AppState,
    build: Option<BuildProjection>,
    subject: &str,
    kind: BuildKind,
) -> Result<Option<BuildProjection>, BuildError> {
    let Some(stale) = build.as_ref().filter(|b| {
        !b.state.is_terminal()
            && biffix_core::clock::epoch_micros() - b.updated.as_micros()
                > kind.stale_after_micros()
    }) else {
        return Ok(build);
    };
    match state.cloud.build_event(&stale.build_arn).await {
        Ok(Some(ev)) => {
            let ev = biffix_core::translate::from_aws_codebuild(&ev);
            state.events.record(&ev).await?;
            tracing::info!(build_arn = stale.build_arn, kind = ?ev.kind(), "builds: stale build reconciled");
            Ok(state.registry.get_build(subject).await?)
        }
        Ok(None) => Ok(build),
        Err(e) => {
            tracing::warn!(build_arn = stale.build_arn, error = %e, "builds: could not look up a stale build");
            Ok(build)
        }
    }
}

/// What a build state change delivered straight to the backend sweep did ([`fold_build_event`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildEventFold {
    /// The payload is no CodeBuild event (the schedule's): nothing to fold.
    NotABuildEvent,
    /// A build biffix did not start: ignored.
    Ignored,
    /// Recorded (a no-op when the ingest path already appended it: same dedup key).
    Recorded,
}

/// Why [`fold_build_event`] folded nothing.
#[derive(Debug, thiserror::Error)]
pub enum BuildEventError {
    /// The event is a CodeBuild state change ingest cannot decode.
    #[error("decoding the build event: {0}")]
    Decode(#[source] biffix_core::eventbridge::decode::DecodeError),
    /// Decoded, but appending it failed.
    #[error("recording the build event: {0}")]
    Record(#[source] StoreError),
}

/// Fold a CodeBuild "Build State Change" event the sweep Lambda was invoked with (an EventBridge
/// rule sends it there as well as to ingest), so the sweep that follows sees the build as it now
/// stands and launches what it finished without waiting for the next scheduled sweep. It is
/// classified and decoded exactly as ingest does
/// ([`biffix_core::eventbridge::codebuild_state_change`]); any other payload, such as the
/// schedule's, folds nothing.
pub async fn fold_build_event(
    state: &AppState,
    payload: &serde_json::Value,
) -> Result<BuildEventFold, BuildEventError> {
    let Some(decoded) = biffix_core::eventbridge::codebuild_state_change(payload) else {
        return Ok(BuildEventFold::NotABuildEvent);
    };
    let Some(ev) = decoded.map_err(BuildEventError::Decode)? else {
        return Ok(BuildEventFold::Ignored);
    };
    state.events.record(&ev).await.map_err(BuildEventError::Record)?;
    Ok(BuildEventFold::Recorded)
}

/// A build failure (any kind of build): its store or AWS cause's kind; an unreadable stored recipe
/// is a corrupt stored value, ours (logged). The message is the build error's own.
impl From<BuildError> for crate::error::ServiceError {
    fn from(err: BuildError) -> Self {
        let message = err.to_string();
        match err {
            BuildError::Store(s) => crate::error::ServiceError::from(s),
            BuildError::Aws(a) => crate::error::ServiceError::from(a),
            BuildError::Recipe(_) => {
                tracing::warn!(error = %message, "build: a stored runtime recipe is unreadable");
                crate::error::ServiceError::Internal(String::new())
            }
        }
        .with_message(message)
    }
}

#[cfg(test)]
mod tests {
    use biffix_core::time::Timestamp;

    use super::*;

    fn build(state: BuildState, reason: Option<&str>) -> BuildProjection {
        BuildProjection {
            subject: "s".into(),
            state,
            build_arn: "arn".into(),
            outputs: Default::default(),
            reason: reason.map(str::to_string),
            logs_url: None,
            request: serde_json::Value::Null,
            builds: AUTO_BUILDS,
            updated: Timestamp::from_micros(0),
        }
    }

    #[test]
    fn a_build_that_reported_nothing_usable_failed() {
        let yes = |_: &BuildProjection| true;
        let no = |_: &BuildProjection| false;
        assert_eq!(phase_of_build(false, None, no), Phase::Unbuilt);
        assert_eq!(phase_of_build(true, None, no), Phase::Ready);
        let ok = build(BuildState::Succeeded, None);
        assert_eq!(phase_of_build(false, Some(&ok), yes), Phase::Building);
        assert_eq!(phase_of_build(false, Some(&ok), no), Phase::Failed);
        assert_eq!(failure_reason(&ok).as_deref(), Some(NO_OUTPUT));
        for s in [BuildState::Requested, BuildState::Running] {
            assert_eq!(phase_of_build(false, Some(&build(s, None)), no), Phase::Building);
        }
        let failed = build(BuildState::Failed, Some("boom"));
        assert_eq!(phase_of_build(false, Some(&failed), yes), Phase::Failed);
        assert_eq!(failure_reason(&failed).as_deref(), Some("boom"));
        assert_eq!(failure_reason(&build(BuildState::Failed, None)), None);
    }

    #[test]
    fn a_start_token_is_per_subject_and_build_count() {
        let t = start_token("runtime:k", 1);
        assert_eq!(t.len(), 64);
        assert_eq!(t, start_token("runtime:k", 1));
        assert_ne!(t, start_token("runtime:k", 2));
        assert_ne!(t, start_token("runtime:j", 1));
        assert_eq!(builds_seen(None), 0);
        assert_eq!(builds_seen(Some(&build(BuildState::Failed, None))), AUTO_BUILDS);
    }

    #[test]
    fn each_kind_of_build_is_stale_after_its_projects_timeout() {
        let minutes = |k: BuildKind| k.stale_after_micros() / 60_000_000;
        assert_eq!(minutes(BuildKind::Runtime), 45);
        assert_eq!(minutes(BuildKind::ArchBuild), 75);
        assert_eq!(minutes(BuildKind::Import), 135);
    }
}
