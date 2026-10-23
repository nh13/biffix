//! How a run's samples stand: the sample x step [`SampleGrid`] built from the run's tasks, its DAG
//! and its state, and the counts the run list shows.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{ReusedSample, SampleSpec, StepScope};
use crate::sample_progress::attribution::{by_step, forced};
use crate::state::{RunState, TaskState};

/// One task, as progress needs it (a `task_projection` row).
#[derive(Clone, Debug, PartialEq)]
pub struct TaskFact {
    pub sample: Option<String>,
    pub group: Option<String>,
    /// The task's state ([`TaskState`]'s spelling: `queued` | `running` | `succeeded` | `failed`).
    pub state: String,
    pub cached: bool,
    pub attempts: i64,
    /// The task reported outputs of its own (a `run_output` credited to it).
    pub outputs: bool,
}

/// How the run stands, as far as its samples care: still going, ended in success, or ended
/// otherwise (failed or cancelled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    Live,
    Succeeded,
    Stopped,
}

impl RunEnd {
    pub fn of(state: RunState) -> RunEnd {
        match state {
            RunState::Succeeded => RunEnd::Succeeded,
            RunState::Failed | RunState::Cancelled => RunEnd::Stopped,
            RunState::PreparingImages | RunState::Submitted | RunState::Running => RunEnd::Live,
        }
    }
}

/// A grid cell: one sample's step (or a whole-study step).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    Waiting,
    Running,
    Done,
    /// Satisfied from an earlier run's results (every task cached).
    Reused,
    Failed,
}

impl CellState {
    /// A task's cell state from its stored state (an unknown spelling reads as waiting).
    pub(super) fn of(state: &str, cached: bool) -> CellState {
        match state.parse::<TaskState>() {
            Ok(TaskState::Succeeded) if cached => CellState::Reused,
            Ok(TaskState::Succeeded) => CellState::Done,
            Ok(TaskState::Failed) => CellState::Failed,
            Ok(TaskState::Running) => CellState::Running,
            Ok(TaskState::Queued) | Err(_) => CellState::Waiting,
        }
    }

    fn finished(self) -> bool {
        matches!(self, CellState::Done | CellState::Reused)
    }

    /// Several tasks in one cell (lanes, chunks): the worst state shows; reused only if all were.
    fn merge(self, other: CellState) -> CellState {
        use CellState::*;
        let rank = |s: CellState| match s {
            Failed => 4,
            Running => 3,
            Waiting => 2,
            Done => 1,
            Reused => 0,
        };
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cell {
    pub state: CellState,
    /// Attempts across the cell's tasks (more than its task count means a retry).
    pub attempts: i64,
    pub tasks: i64,
}

impl Cell {
    fn add(&mut self, state: CellState, attempts: i64) {
        self.state = self.state.merge(state);
        self.attempts += attempts;
        self.tasks += 1;
    }
}

/// How one sample stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SampleState {
    /// No task yet (the run is still going).
    Waiting,
    Running,
    Done,
    Failed,
    /// Did not finish because the run ended: the run failed or was cancelled first, or (a sheet
    /// sample no task ever named) it never ran.
    Stopped,
}

impl SampleState {
    /// Every state, for exhaustive checks.
    pub const ALL: [SampleState; 5] = [
        SampleState::Waiting,
        SampleState::Running,
        SampleState::Done,
        SampleState::Failed,
        SampleState::Stopped,
    ];

    /// The wire spelling (the serde name).
    pub const fn as_str(self) -> &'static str {
        match self {
            SampleState::Waiting => "waiting",
            SampleState::Running => "running",
            SampleState::Done => "done",
            SampleState::Failed => "failed",
            SampleState::Stopped => "stopped",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    pub group: String,
    /// The author's label for the step (`samples.labels`), else one made from the group name.
    pub label: String,
    pub scope: StepScope,
    /// `label` is the author's, not one made from the group name: a client may word a made one
    /// its own way, but shows an authored one as written. Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub authored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SampleRow {
    pub id: String,
    pub state: SampleState,
    /// Keyed by step group; a step the sample has no task in yet is absent.
    pub cells: BTreeMap<String, Cell>,
}

/// A run's samples × steps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SampleGrid {
    /// Steps in pipeline order.
    pub steps: Vec<Step>,
    /// Samples in the samplesheet's order, then any others seen, by id.
    pub samples: Vec<SampleRow>,
    /// Whole-study steps' cells, keyed by group.
    pub study: BTreeMap<String, Cell>,
    /// Tasks in a per-sample step that name no sample (biffix could not match them).
    pub unattributed: i64,
    /// The run's samples are the samplesheet's (`true`), or only those tasks named so far.
    pub declared: bool,
}

/// How a run's samples stand, for a status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleCounts {
    /// With a sheet (`declared`): the sheet's samples plus any other sample a task named, once a
    /// task has named one (0 until then: the grid shows no sample yet); else the samples tasks
    /// have named so far.
    pub total: i64,
    pub done: i64,
    pub failed: i64,
    /// Whether the run has a sheet (`false`: `total` is the samples seen so far).
    pub declared: bool,
}

impl SampleGrid {
    /// Build the grid from the run's tasks. `dag` is the run's group graph (nodes, edges), which
    /// orders the steps and tells whether every step has started; `end` is how the run stands.
    ///
    /// A sample is done once every task it has finished and nothing more can come for it: the run
    /// succeeded, or the sample has a task in every per-sample step and every step of a known DAG
    /// has started. (A sample need not pass through every per-sample step: in a workflow whose
    /// samples take different paths, it is done when the run succeeds. Nextflow reports its DAG
    /// only when the flow completes, so a live Nextflow run settles no sample early.) Once the run
    /// failed or was cancelled, a sample neither failed nor done is stopped.
    pub fn build(
        spec: &SampleSpec,
        tasks: &[TaskFact],
        dag: (&BTreeSet<String>, &BTreeSet<(String, String)>),
        end: RunEnd,
    ) -> SampleGrid {
        // The resumed-from run's finished cells this run can show (see `SampleSpec::reused`): with
        // a sheet, only its samples; never a step the author forced to `study`.
        let sheet_ids: BTreeSet<&str> = spec.ids.iter().map(String::as_str).collect();
        let reused: Vec<&ReusedSample> = spec
            .reused
            .iter()
            .filter(|r| sheet_ids.is_empty() || sheet_ids.contains(r.id.as_str()))
            .collect();
        let reused_cell = |group: &str| forced(&spec.hint, group) != Some(&StepScope::Study);
        let mut groups: BTreeSet<&str> = tasks.iter().filter_map(|t| t.group.as_deref()).collect();
        let mut attributed: BTreeSet<&str> = tasks
            .iter()
            .filter(|t| t.sample.is_some())
            .filter_map(|t| t.group.as_deref())
            .collect();
        for g in reused
            .iter()
            .flat_map(|r| r.steps.keys())
            .map(String::as_str)
            .filter(|g| reused_cell(g))
        {
            groups.insert(g);
            attributed.insert(g);
        }
        // Per-sample: a step with an attributed task, or one the author forced to `sample`; never
        // one forced to `study`.
        let per_sample: BTreeSet<&str> = groups
            .iter()
            .copied()
            .filter(|g| match forced(&spec.hint, g) {
                Some(StepScope::Study) => false,
                Some(StepScope::Sample) => true,
                _ => attributed.contains(g),
            })
            .collect();

        let mut cells: BTreeMap<&str, BTreeMap<&str, Cell>> = BTreeMap::new();
        let mut study: BTreeMap<String, Cell> = BTreeMap::new();
        let mut unattributed = 0;
        for t in tasks {
            let Some(group) = t.group.as_deref() else {
                continue;
            };
            let state = CellState::of(&t.state, t.cached);
            let empty = Cell { state, attempts: 0, tasks: 0 };
            let cell = match (&t.sample, per_sample.contains(group)) {
                (Some(s), true) => {
                    cells.entry(s.as_str()).or_default().entry(group).or_insert(empty)
                }
                (None, true) => {
                    unattributed += 1;
                    continue;
                }
                _ => study.entry(group.to_string()).or_insert(empty),
            };
            cell.add(state, t.attempts);
        }
        // The resumed-from run's finished cells, where this run has none of its own: reused (no
        // attempt was made here). A cell this run has (a Nextflow resume reports its cached tasks)
        // is this run's, so nothing is counted twice. A sample with no task here that was done
        // there is done; one that was not has only carried cells until its first task here.
        let mut done_before: BTreeSet<&str> = BTreeSet::new();
        let mut carried_only: BTreeSet<&str> = BTreeSet::new();
        for r in &reused {
            let row = cells.entry(r.id.as_str()).or_default();
            let own_tasks = !row.is_empty();
            if !own_tasks {
                carried_only.insert(r.id.as_str());
            }
            for (group, &n) in r.steps.iter().filter(|(g, _)| reused_cell(g)) {
                row.entry(group.as_str()).or_insert(Cell {
                    state: CellState::Reused,
                    attempts: 0,
                    tasks: n,
                });
            }
            if !own_tasks && r.done && !row.is_empty() {
                done_before.insert(r.id.as_str());
            }
        }
        cells.retain(|_, row| !row.is_empty());

        let all_started = !dag.0.is_empty() && dag.0.iter().all(|n| groups.contains(n.as_str()));
        let state_of = |row: Option<&BTreeMap<&str, Cell>>| {
            let Some(row) = row else {
                return match end {
                    RunEnd::Live => SampleState::Waiting,
                    RunEnd::Succeeded | RunEnd::Stopped => SampleState::Stopped,
                };
            };
            if row.values().any(|c| c.state == CellState::Failed) {
                return SampleState::Failed;
            }
            let settled = end == RunEnd::Succeeded
                || (all_started && per_sample.iter().all(|g| row.contains_key(g)));
            if row.values().all(|c| c.state.finished()) && settled {
                SampleState::Done
            } else if end == RunEnd::Live {
                SampleState::Running
            } else {
                SampleState::Stopped
            }
        };
        let row_of = |id: &str| SampleRow {
            id: id.to_string(),
            state: if done_before.contains(id) {
                SampleState::Done
            } else if carried_only.contains(id) && end == RunEnd::Live {
                // Its finished steps are the earlier run's; nothing of it has started here yet.
                SampleState::Waiting
            } else {
                state_of(cells.get(id))
            },
            cells: cells
                .get(id)
                .map(|r| r.iter().map(|(g, c)| (g.to_string(), c.clone())).collect())
                .unwrap_or_default(),
        };
        // Samples show only once a task has been matched to one: until then (or ever, for a workflow
        // whose task keys name no sample) the sheet's ids would sit "waiting" with nothing behind
        // them, so the run reads by steps instead.
        let sheet: &[String] = if cells.is_empty() { &[] } else { &spec.ids };
        let samples: Vec<SampleRow> =
            sample_ids(sheet, cells.keys().copied()).iter().map(|id| row_of(id)).collect();

        // The workflow's target rule (Snakemake's `all`) is no step a person waits on: hide it.
        let hidden = target_steps(tasks, &per_sample, dag);
        study.retain(|g, _| !hidden.contains(g.as_str()));
        let steps = step_order(&groups, dag)
            .into_iter()
            .filter(|g| !hidden.contains(g))
            .map(|g| {
                let authored = by_step(&spec.hint.labels, g).cloned();
                Step {
                    authored: authored.is_some(),
                    label: authored.unwrap_or_else(|| default_label(g)),
                    scope: if per_sample.contains(g) {
                        StepScope::Sample
                    } else {
                        StepScope::Study
                    },
                    group: g.to_string(),
                }
            })
            .collect();
        SampleGrid { steps, samples, study, unattributed, declared: !spec.ids.is_empty() }
    }

    /// What a run resuming this one carries of it ([`SampleSpec::reused`]): each sample's finished
    /// per-sample cells (done or reused here) and whether it was done. Samples with no finished
    /// cell are left out.
    pub fn reused_by_resume(&self) -> Vec<ReusedSample> {
        self.samples
            .iter()
            .filter_map(|row| {
                let steps: BTreeMap<String, i64> = row
                    .cells
                    .iter()
                    .filter(|(_, c)| c.state.finished())
                    .map(|(g, c)| (g.clone(), c.tasks))
                    .collect();
                (!steps.is_empty()).then(|| ReusedSample {
                    id: row.id.clone(),
                    done: row.state == SampleState::Done,
                    steps,
                })
            })
            .collect()
    }

    /// Done and failed samples of the grid's (stopped and waiting ones count toward neither).
    pub fn counts(&self) -> SampleCounts {
        let n = |s: SampleState| self.samples.iter().filter(|r| r.state == s).count() as i64;
        SampleCounts {
            total: self.samples.len() as i64,
            done: n(SampleState::Done),
            failed: n(SampleState::Failed),
            declared: self.declared,
        }
    }
}

/// The whole-study steps that are the workflow's target rather than work (Snakemake's `all`, the
/// default target, which only names the files the run must end with): a sink of the run's DAG (it
/// consumes other steps and feeds none), every task of it local (never a Batch job: no attempt, not
/// cached), none failed, and none reporting outputs of its own. Each condition is needed: a local
/// step that writes a report has outputs once it ends, and a failure always shows. Without a known
/// DAG nothing is hidden.
fn target_steps<'a>(
    tasks: &'a [TaskFact],
    per_sample: &BTreeSet<&str>,
    (nodes, edges): (&BTreeSet<String>, &BTreeSet<(String, String)>),
) -> BTreeSet<&'a str> {
    let mut by_group: BTreeMap<&str, Vec<&TaskFact>> = BTreeMap::new();
    for t in tasks {
        if let Some(g) = t.group.as_deref() {
            by_group.entry(g).or_default().push(t);
        }
    }
    by_group
        .into_iter()
        .filter(|(g, ts)| {
            let sink = nodes.contains(*g)
                && edges.iter().any(|(_, to)| to == g)
                && !edges.iter().any(|(from, _)| from == g);
            sink && !per_sample.contains(g)
                && ts.iter().all(|t| {
                    t.attempts == 0
                        && !t.cached
                        && !t.outputs
                        && CellState::of(&t.state, false) != CellState::Failed
                })
        })
        .map(|(g, _)| g)
        .collect()
}

/// A run's sample ids in the samples grid's order: the `sheet`'s ids in sheet order (a repeated id
/// once), then the other `attributed` ids (the samples its tasks were matched to that the sheet does
/// not list), sorted, each once.
pub fn sample_ids<'a, I>(sheet: &[String], attributed: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    let mut ids: Vec<String> =
        sheet.iter().filter(|id| listed.insert(id.as_str())).cloned().collect();
    let others: BTreeSet<&str> = attributed.into_iter().filter(|id| !listed.contains(id)).collect();
    ids.extend(others.into_iter().map(str::to_string));
    ids
}

/// A step's name for display: its last `:`-separated part (a Nextflow process path), `_` → space.
fn default_label(group: &str) -> String {
    group.rsplit(':').next().unwrap_or(group).replace('_', " ")
}

/// The steps in pipeline order: a topological order of the DAG (ties by name), then any group the
/// DAG does not name, by name.
///
/// Not shared with `backend::provenance::step_order`, which differs on purpose: provenance breaks
/// ties first-in-first-out over the DAG's node order and seeds task-only groups as roots, and its
/// order is frozen into published provenance documents; the grid wants a stable by-name order among
/// ready steps and the DAG-less groups last.
fn step_order<'a>(
    groups: &BTreeSet<&'a str>,
    (nodes, edges): (&'a BTreeSet<String>, &'a BTreeSet<(String, String)>),
) -> Vec<&'a str> {
    let mut indegree: BTreeMap<&str, usize> = nodes.iter().map(|n| (n.as_str(), 0)).collect();
    // Only an edge from a node counts: one from outside the graph is never taken off again, so it
    // would hold its target (and all after it) back for the cycle fallback.
    for (_, to) in edges.iter().filter(|(from, _)| nodes.contains(from)) {
        if let Some(d) = indegree.get_mut(to.as_str()) {
            *d += 1;
        }
    }
    let mut ready: BTreeSet<&str> =
        indegree.iter().filter(|(_, d)| **d == 0).map(|(n, _)| *n).collect();
    let mut order: Vec<&str> = Vec::new();
    while let Some(n) = ready.pop_first() {
        order.push(n);
        for (from, to) in edges {
            if from == n {
                if let Some(d) = indegree.get_mut(to.as_str()) {
                    *d -= 1;
                    if *d == 0 {
                        ready.insert(to.as_str());
                    }
                }
            }
        }
    }
    // A cycle leaves nodes unordered: append them by name rather than drop them.
    let placed: BTreeSet<&str> = order.iter().copied().collect();
    order.extend(nodes.iter().map(String::as_str).filter(|n| !placed.contains(n)));
    let mut out: Vec<&'a str> = order.into_iter().filter(|g| groups.contains(g)).collect();
    let seen: BTreeSet<&str> = out.iter().copied().collect();
    out.extend(groups.iter().copied().filter(|g| !seen.contains(g)));
    out
}
