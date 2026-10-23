//! [`CostReads`]: what runs cost, and the past runs a launch estimate and a typical run read.

use std::collections::HashMap;

use async_trait::async_trait;
use biffix_core::ids::RunId;

use super::StoreError;

/// One priced job of a run (an `attempt_cost` row).
#[derive(Clone, Debug, PartialEq)]
pub struct CostRow {
    pub batch_job_id: String,
    /// The job was the run's coordinator (`attempt_cost.role`), not a task attempt.
    pub coordinator: bool,
    /// The task the job was an attempt of. `None` for the coordinator, and for a task job whose
    /// attempt row is gone (a replay that re-keyed or dropped it): that job still counts in the
    /// run's gross cost, but in neither its coordinator nor its winning cost.
    pub task_key: Option<String>,
    pub instance_type: String,
    pub lifecycle: Option<String>,
    pub hours: f64,
    pub price_per_hour: f64,
    pub price_source: String,
    pub compute_usd: f64,
    pub ebs_usd: f64,
}

impl CostRow {
    /// Compute plus EBS cost, in USD.
    pub(crate) fn usd(&self) -> f64 {
        self.compute_usd + self.ebs_usd
    }
}

/// A run's cost as a list row shows it (`RunSummary::cost_usd` and `cost_pending`): its estimate
/// once a job is priced ([`biffix_api::RunCost::priced_usd`]), and whether jobs are still to be
/// priced (the estimate is not final yet).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ListCost {
    pub usd: Option<f64>,
    pub pending: bool,
}

impl ListCost {
    /// The list's view of a run detail's `cost`: what [`CostReads::run_costs`] gives for
    /// the run, so the list and the run page agree.
    pub fn from_run_cost(cost: Option<&biffix_api::RunCost>) -> Self {
        cost.map_or_else(Self::default, |c| Self {
            usd: c.priced_usd(),
            pending: c.pending_jobs > 0,
        })
    }
}

/// Roll a run's priced jobs up into its [`biffix_api::RunCost`]. `final_attempts` are the job ids of
/// each task's display (final) attempt — the winning cost counts only those plus the coordinator;
/// `pending_jobs` counts the run's jobs not priced yet that still can be. `None` when nothing is
/// priced or pending.
pub(crate) fn run_cost(
    rows: &[CostRow],
    final_attempts: &std::collections::BTreeSet<String>,
    pending_jobs: i64,
) -> Option<biffix_api::RunCost> {
    if rows.is_empty() && pending_jobs == 0 {
        return None;
    }
    let mut by_type: std::collections::BTreeMap<(String, Option<String>), (i64, f64, f64)> =
        Default::default();
    let mut sources = std::collections::BTreeSet::new();
    let (mut compute, mut ebs, mut coordinator, mut winning) = (0.0, 0.0, 0.0, 0.0);
    for row in rows {
        compute += row.compute_usd;
        ebs += row.ebs_usd;
        if row.coordinator {
            coordinator += row.usd();
            winning += row.usd();
        } else if final_attempts.contains(&row.batch_job_id) {
            winning += row.usd();
        }
        sources.insert(row.price_source.clone());
        let entry = by_type.entry((row.instance_type.clone(), row.lifecycle.clone())).or_default();
        entry.0 += 1;
        entry.1 += row.hours;
        entry.2 += row.usd();
    }
    Some(biffix_api::RunCost {
        estimated_usd: compute + ebs,
        compute_usd: compute,
        ebs_usd: ebs,
        coordinator_usd: coordinator,
        winning_usd: winning,
        priced_jobs: rows.len() as i64,
        pending_jobs,
        price_sources: sources.into_iter().collect(),
        by_instance_type: by_type
            .into_iter()
            .map(|((instance_type, lifecycle), (jobs, hours, usd))| {
                biffix_api::CostByInstanceType { instance_type, lifecycle, jobs, hours, usd }
            })
            .collect(),
    })
}

/// Run costs (the priced jobs' `attempt_cost`), usage over a period, and the past runs a launch
/// estimate or a workflow's typical run is based on.
#[async_trait]
pub trait CostReads: Send + Sync {
    /// Each of `run_ids`' cost as the run list shows it ([`ListCost::from_run_cost`] of its
    /// detail's `cost`, the estimate bit-for-bit), by run id, in one read for a page of the list
    /// rather than one per run. A run with no cost (nothing priced or pending), or not found, is
    /// absent.
    async fn run_costs(&self, run_ids: &[RunId]) -> Result<HashMap<String, ListCost>, StoreError>;

    /// Estimated spend of the runs submitted in `[from_micros, to_micros)`, grouped by `group`
    /// (largest first; by month, oldest first), with the period's total. Only runs with a cost.
    async fn usage(
        &self,
        from_micros: i64,
        to_micros: i64,
        group: crate::runs::usage::UsageGroup,
    ) -> Result<crate::runs::usage::UsageReport, StoreError>;

    /// The most recent `limit` successful runs of a workflow (by project and name, any version) that
    /// are fully priced and have a sample count: the basis of a launch estimate. Newest first. Only
    /// fresh runs that launched count: a resumed run (it skipped cached work) and a run with no
    /// launch time are left out, as is one with a job still pending pricing.
    async fn estimate_basis(
        &self,
        project: &str,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::runs::estimate::PastRun>, StoreError>;

    /// The most recent `limit` successful runs of a workflow (by project and name, any version),
    /// with or without a sample count: the basis of a typical run (its usual run time, and its
    /// cost, `None` for a run not fully priced yet). Newest first. Resumed runs and runs with no
    /// launch time are left out, as from [`Self::estimate_basis`].
    async fn typical_basis(
        &self,
        project: &str,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::runs::estimate::PastWall>, StoreError>;

    /// [`Self::typical_basis`] of every workflow at once, by (project, name), in one read: what the
    /// workflow list needs to say each pipeline's typical run. A workflow with no such run is absent.
    async fn typical_bases(
        &self,
        limit: i64,
    ) -> Result<HashMap<(String, String), Vec<crate::runs::estimate::PastWall>>, StoreError>;
}

#[cfg(test)]
mod cost_tests {
    use super::{run_cost, CostRow};
    use biffix_api::CostByInstanceType;

    /// A task attempt's cost row (`None` task: its attempt row is gone), on spot.
    fn row(job: &str, task: Option<&str>, itype: &str, usd: f64) -> CostRow {
        CostRow {
            batch_job_id: job.into(),
            coordinator: false,
            task_key: task.map(String::from),
            instance_type: itype.into(),
            lifecycle: Some("spot".into()),
            hours: 1.0,
            price_per_hour: usd,
            price_source: "spot_history".into(),
            compute_usd: usd * 0.75,
            ebs_usd: usd * 0.25,
        }
    }

    /// The coordinator's cost row, on demand.
    fn coordinator(job: &str, itype: &str, usd: f64) -> CostRow {
        CostRow {
            coordinator: true,
            lifecycle: Some("on_demand".into()),
            price_source: "on_demand".into(),
            ..row(job, None, itype, usd)
        }
    }

    #[test]
    fn gross_counts_every_attempt_and_winning_only_final_ones_plus_the_coordinator() {
        let rows = vec![
            row("a1", Some("align"), "c6a.4xlarge", 1.0), // spot-lost first attempt
            row("a2", Some("align"), "c6a.4xlarge", 1.0), // final attempt
            row("m1", Some("merge"), "c7i.4xlarge", 2.0),
            coordinator("coord", "m6i.large", 0.5),
        ];
        let finals = ["a2".to_string(), "m1".to_string()].into_iter().collect();
        let cost = run_cost(&rows, &finals, 1).unwrap();
        assert!((cost.estimated_usd - 4.5).abs() < 1e-9);
        assert!((cost.compute_usd + cost.ebs_usd - 4.5).abs() < 1e-9);
        assert!((cost.winning_usd - 3.5).abs() < 1e-9);
        assert!((cost.coordinator_usd - 0.5).abs() < 1e-9);
        assert_eq!((cost.priced_jobs, cost.pending_jobs), (4, 1));
        assert_eq!(cost.price_sources, vec!["on_demand", "spot_history"]);
    }

    #[test]
    fn jobs_are_grouped_by_instance_type_and_lifecycle() {
        let rows = vec![
            row("a1", Some("align"), "c6a.4xlarge", 1.0),
            CostRow { hours: 2.0, ..row("a2", Some("align"), "c6a.4xlarge", 1.0) },
            // The same type on demand is its own group.
            CostRow {
                lifecycle: Some("on_demand".into()),
                price_source: "on_demand".into(),
                ..row("a3", Some("align"), "c6a.4xlarge", 4.0)
            },
            row("m1", Some("merge"), "c7i.4xlarge", 2.0),
            coordinator("coord", "m6i.large", 0.5),
        ];
        let cost = run_cost(&rows, &Default::default(), 0).unwrap();
        let group = |itype: &str, lifecycle: &str, jobs, hours, usd| CostByInstanceType {
            instance_type: itype.into(),
            lifecycle: Some(lifecycle.into()),
            jobs,
            hours,
            usd,
        };
        assert_eq!(
            cost.by_instance_type,
            vec![
                group("c6a.4xlarge", "on_demand", 1, 1.0, 4.0),
                group("c6a.4xlarge", "spot", 2, 3.0, 2.0),
                group("c7i.4xlarge", "spot", 1, 1.0, 2.0),
                group("m6i.large", "on_demand", 1, 1.0, 0.5),
            ]
        );
        // Each source once, sorted.
        assert_eq!(cost.price_sources, vec!["on_demand", "spot_history"]);
    }

    #[test]
    fn a_task_job_without_its_attempt_is_gross_only_never_the_coordinator() {
        // A replay dropped the attempt row of `orphan`: it has no task, but it is not the
        // coordinator either.
        let rows =
            vec![coordinator("coord", "m6i.large", 0.1), row("orphan", None, "c6a.4xlarge", 5.0)];
        let cost = run_cost(&rows, &Default::default(), 0).unwrap();
        assert!((cost.estimated_usd - 5.1).abs() < 1e-9, "{cost:?}");
        assert!((cost.coordinator_usd - 0.1).abs() < 1e-9, "{cost:?}");
        assert!((cost.winning_usd - 0.1).abs() < 1e-9, "{cost:?}");
    }

    #[test]
    fn no_cost_until_something_is_priced_or_pending() {
        assert_eq!(run_cost(&[], &Default::default(), 0), None);
        let pending_only = run_cost(&[], &Default::default(), 2).unwrap();
        assert_eq!(pending_only.estimated_usd, 0.0);
        assert_eq!(pending_only.pending_jobs, 2);
    }
}
