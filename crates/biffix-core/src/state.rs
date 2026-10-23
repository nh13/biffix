//! The lifecycle states the fold derives for an attempt, a task and a run, with their stored
//! spellings (each parses back from `as_str` through `FromStr`).
//!
//! Their own module, below both the reducer that derives them and the per-sample progress that
//! reads them, so neither has to depend on the other for the vocabulary.

/// Attempt lifecycle. Monotonic; never regresses.
///
/// Deliberately not `Ord`: monotonicity is enforced through `AttemptState::rank`, whose order
/// (`Succeeded` outranks `Failed`) differs from this declaration order — a derived comparison would
/// be a silent foot-gun.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptState {
    Created,
    Submitted,
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl AttemptState {
    /// Every state, in declaration order: what `FromStr` parses.
    pub const ALL: [AttemptState; 6] = [
        AttemptState::Created,
        AttemptState::Submitted,
        AttemptState::Queued,
        AttemptState::Running,
        AttemptState::Succeeded,
        AttemptState::Failed,
    ];

    /// Ordinal used only to enforce monotonicity. `Succeeded` outranks `Failed` so that when one
    /// attempt somehow carries both terminals (at most one of each — the AWS dedup key is
    /// `(job, status)`), *success wins* regardless of the order the two are folded: `advance_to`
    /// moves Failed → Succeeded but never the reverse. This keeps the attempt's state a pure
    /// function of the observed event set, which a shared top rank did not.
    pub(crate) fn rank(self) -> u8 {
        match self {
            AttemptState::Created => 0,
            AttemptState::Submitted => 1,
            AttemptState::Queued => 2,
            AttemptState::Running => 3,
            AttemptState::Failed => 4,
            AttemptState::Succeeded => 5,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, AttemptState::Succeeded | AttemptState::Failed)
    }

    /// The canonical lowercase name persisted on `attempt_projection.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Created => "created",
            AttemptState::Submitted => "submitted",
            AttemptState::Queued => "queued",
            AttemptState::Running => "running",
            AttemptState::Succeeded => "succeeded",
            AttemptState::Failed => "failed",
        }
    }
}

impl std::fmt::Display for AttemptState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AttemptState {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AttemptState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_string()))
    }
}

/// Task lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// The task has live Batch attempt(s) but none has started executing yet — every attempt is
    /// still `Created`/`Submitted`/`Queued` in Batch (typically `RUNNABLE`, waiting on capacity).
    /// Distinct from `Running` so the UI does not show a queued job as running.
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl TaskState {
    /// Every state, in declaration order: what `FromStr` parses.
    pub const ALL: [TaskState; 4] =
        [TaskState::Queued, TaskState::Running, TaskState::Succeeded, TaskState::Failed];

    /// The canonical projection/DB string for this state — the single source of truth every
    /// consumer (the fold's persistence, SQL predicates, filters) derives from.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Succeeded => "succeeded",
            TaskState::Failed => "failed",
        }
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TaskState {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TaskState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_string()))
    }
}

/// Run lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Accepted, waiting for its submit-time images before it launches.
    PreparingImages,
    Submitted,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Returned when a stored state string does not correspond to a known variant. Carries the raw
/// value so a caller can log exactly what it saw. The one parse error of every stored-state enum
/// ([`AttemptState`], [`TaskState`], [`RunState`], [`crate::builds::BuildState`]), each of which
/// parses through `FromStr` as the inverse of its `as_str`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown state {0:?}")]
pub struct UnknownState(pub String);

impl RunState {
    /// Every run state, in lifecycle order. The one place the set of states is enumerated, so
    /// filters and SQL predicates cannot fall out of sync with the type.
    pub const ALL: [RunState; 6] = [
        RunState::PreparingImages,
        RunState::Submitted,
        RunState::Running,
        RunState::Succeeded,
        RunState::Failed,
        RunState::Cancelled,
    ];

    /// The canonical projection/DB string for this state — the single source of truth every
    /// consumer (the fold's persistence, SQL predicates, run filters) derives from.
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::PreparingImages => "preparing_images",
            RunState::Submitted => "submitted",
            RunState::Running => "running",
            RunState::Succeeded => "succeeded",
            RunState::Failed => "failed",
            RunState::Cancelled => "cancelled",
        }
    }

    /// Whether this is a terminal run state (no further transitions).
    pub fn is_terminal(self) -> bool {
        matches!(self, RunState::Succeeded | RunState::Failed | RunState::Cancelled)
    }
}

impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RunState {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `all` lists every variant once, in the order `position` (an exhaustive `match`, so a new
    /// variant fails to compile until it is placed) gives, and each parses back from `as_str`.
    fn pins<T: Copy + PartialEq + std::fmt::Debug + std::str::FromStr>(
        all: &[T],
        position: fn(T) -> usize,
        as_str: fn(T) -> &'static str,
    ) {
        for (i, &state) in all.iter().enumerate() {
            assert_eq!(position(state), i, "{state:?}");
            assert!(as_str(state).parse::<T>().ok() == Some(state), "{state:?}");
        }
        assert!("not-a-state".parse::<T>().is_err());
    }

    #[test]
    fn every_state_is_listed_in_order_and_parses_back() {
        pins(
            &AttemptState::ALL,
            |s| match s {
                AttemptState::Created => 0,
                AttemptState::Submitted => 1,
                AttemptState::Queued => 2,
                AttemptState::Running => 3,
                AttemptState::Succeeded => 4,
                AttemptState::Failed => 5,
            },
            AttemptState::as_str,
        );
        pins(
            &TaskState::ALL,
            |s| match s {
                TaskState::Queued => 0,
                TaskState::Running => 1,
                TaskState::Succeeded => 2,
                TaskState::Failed => 3,
            },
            TaskState::as_str,
        );
        // A run's states in lifecycle order.
        pins(
            &RunState::ALL,
            |s| match s {
                RunState::PreparingImages => 0,
                RunState::Submitted => 1,
                RunState::Running => 2,
                RunState::Succeeded => 3,
                RunState::Failed => 4,
                RunState::Cancelled => 5,
            },
            RunState::as_str,
        );
        assert_eq!(RunState::PreparingImages.as_str(), "preparing_images");
        assert!(!RunState::PreparingImages.is_terminal());
    }
}
