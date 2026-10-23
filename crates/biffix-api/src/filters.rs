//! `filters` — the state filters a client picks runs and tasks by: the runs list's `filter` and the
//! Jobs view's `state` chips. Shared so the CLI's `--state`, the UI's chips and the backend's
//! queries name the same set.
//!
//! The requests carry these as strings (`RunQueryRequest.filter`, `TaskQueryRequest.state`), parsed
//! with [`RunFilter::parse`] / [`TaskStateFilter::parse`] only once the caller may see what they
//! filter, so a bad value never tells an unauthorized caller anything; the OpenAPI document still
//! publishes the accepted set as these enums.

use serde::{Deserialize, Serialize};

use biffix_core::state::{RunState, TaskState};

/// Which runs by state: the runs list's filter (`GET /runs?filter=`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum RunFilter {
    /// Every run.
    #[default]
    All,
    /// Preparing images, submitted or running.
    Active,
    /// Submitted, not yet running.
    Submitted,
    /// Succeeded, failed or cancelled.
    Completed,
    Succeeded,
    Failed,
}

impl RunFilter {
    /// Every filter, in the order a client lists them.
    pub const ALL: &'static [RunFilter] = &[
        RunFilter::All,
        RunFilter::Active,
        RunFilter::Submitted,
        RunFilter::Completed,
        RunFilter::Succeeded,
        RunFilter::Failed,
    ];

    /// The accepted spellings, for a caller's error message.
    pub const ACCEPTED: &'static str = "all, active, submitted, completed, succeeded or failed";

    /// The wire spelling (the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            RunFilter::All => "all",
            RunFilter::Active => "active",
            RunFilter::Submitted => "submitted",
            RunFilter::Completed => "completed",
            RunFilter::Succeeded => "succeeded",
            RunFilter::Failed => "failed",
        }
    }

    /// The filter spelled `s`, or `None` for a spelling no filter has.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|f| f.as_str() == s)
    }

    /// The run states this filter admits, or `None` for "any state" (`All`). This is the single
    /// definition of each filter's membership; both [`RunFilter::matches`] (Rust) and the backend's
    /// SQL predicate derive from it, so they cannot drift from each other or from [`RunState`].
    pub fn states(self) -> Option<&'static [RunState]> {
        use RunState::{Cancelled, Failed, PreparingImages, Running, Submitted, Succeeded};
        Some(match self {
            RunFilter::All => return None,
            RunFilter::Active => &[PreparingImages, Submitted, Running],
            RunFilter::Submitted => &[Submitted],
            RunFilter::Completed => &[Succeeded, Failed, Cancelled],
            RunFilter::Succeeded => &[Succeeded],
            RunFilter::Failed => &[Failed],
        })
    }

    /// Whether a run in `state` (the projection's stored state string) matches this filter. An
    /// unrecognized state string never matches a specific filter (only `All`).
    pub fn matches(self, state: &str) -> bool {
        match self.states() {
            None => true,
            Some(states) => state.parse::<RunState>().is_ok_and(|parsed| states.contains(&parsed)),
        }
    }
}

/// Which tasks by state: the Jobs view's filter chips (the `state` of
/// `POST /runs/{id}/tasks/query`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum TaskStateFilter {
    #[default]
    All,
    /// Running or queued.
    Active,
    Succeeded,
    Failed,
}

impl TaskStateFilter {
    /// Every filter, in the order the chips show them.
    pub const ALL: &'static [TaskStateFilter] = &[
        TaskStateFilter::All,
        TaskStateFilter::Active,
        TaskStateFilter::Succeeded,
        TaskStateFilter::Failed,
    ];

    /// The accepted spellings, for a caller's error message.
    pub const ACCEPTED: &'static str = "all, active, succeeded or failed";

    /// The wire spelling (the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStateFilter::All => "all",
            TaskStateFilter::Active => "active",
            TaskStateFilter::Succeeded => "succeeded",
            TaskStateFilter::Failed => "failed",
        }
    }

    /// The filter spelled `s`, or `None` for a spelling no filter has.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|f| f.as_str() == s)
    }

    /// Whether a task in `state` shows under this filter. Exhaustive over [`TaskState`], so a new
    /// state fails to compile until it is placed here, rather than silently dropping out of a chip.
    pub fn includes(self, state: TaskState) -> bool {
        let active = match state {
            TaskState::Queued | TaskState::Running => true,
            TaskState::Succeeded | TaskState::Failed => false,
        };
        match self {
            Self::All => true,
            Self::Active => active,
            Self::Succeeded => state == TaskState::Succeeded,
            Self::Failed => state == TaskState::Failed,
        }
    }

    /// [`Self::includes`] for a projected state string; one no [`TaskState`] spells shows only
    /// under [`Self::All`].
    pub fn matches(self, state: &str) -> bool {
        match state.parse::<TaskState>() {
            Ok(state) => self.includes(state),
            Err(_) => self == Self::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `spellings` as an error message lists them: `a, b, … or z`.
    fn listed(spellings: &[&str]) -> String {
        match spellings.split_last() {
            Some((last, [])) => (*last).to_string(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => String::new(),
        }
    }

    #[test]
    fn every_filter_is_listed_once_in_order() {
        // An exhaustive match: a new filter fails to compile here until it is placed in `ALL`.
        let position = |f: RunFilter| match f {
            RunFilter::All => 0,
            RunFilter::Active => 1,
            RunFilter::Submitted => 2,
            RunFilter::Completed => 3,
            RunFilter::Succeeded => 4,
            RunFilter::Failed => 5,
        };
        for (i, &f) in RunFilter::ALL.iter().enumerate() {
            assert_eq!(position(f), i, "{f:?}");
        }
        let position = |f: TaskStateFilter| match f {
            TaskStateFilter::All => 0,
            TaskStateFilter::Active => 1,
            TaskStateFilter::Succeeded => 2,
            TaskStateFilter::Failed => 3,
        };
        for (i, &f) in TaskStateFilter::ALL.iter().enumerate() {
            assert_eq!(position(f), i, "{f:?}");
        }
    }

    #[test]
    fn the_accepted_spellings_are_every_filter_s() {
        let runs: Vec<&str> = RunFilter::ALL.iter().map(|f| f.as_str()).collect();
        assert_eq!(RunFilter::ACCEPTED, listed(&runs));
        let tasks: Vec<&str> = TaskStateFilter::ALL.iter().map(|f| f.as_str()).collect();
        assert_eq!(TaskStateFilter::ACCEPTED, listed(&tasks));
    }

    #[test]
    fn spellings_match_serde_and_parse_back() {
        for &f in RunFilter::ALL {
            let wire = serde_json::to_value(f).unwrap();
            assert_eq!(wire.as_str(), Some(f.as_str()));
            assert_eq!(RunFilter::parse(f.as_str()), Some(f));
        }
        for &f in TaskStateFilter::ALL {
            let wire = serde_json::to_value(f).unwrap();
            assert_eq!(wire.as_str(), Some(f.as_str()));
            assert_eq!(TaskStateFilter::parse(f.as_str()), Some(f));
        }
        assert_eq!(RunFilter::parse("bogus"), None);
        assert_eq!(RunFilter::parse(""), None);
        assert_eq!(TaskStateFilter::parse("Active"), None);
    }
}
