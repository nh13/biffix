//! [`MemEventSink`]: the in-memory [`EventSink`].

use std::sync::{Arc, Mutex};

use crate::store::{EventSink, StoreError};
use async_trait::async_trait;
use biffix_core::event::RunEvent;
use biffix_core::ids::{CoordinatorJobId, RunId};

use super::{run_detail, MemRegistry, MemStore};

/// In-memory [`EventSink`] double: records appended events for assertions, deduping on `dedup_key`
/// exactly like the real store so the ingest route's appended-vs-duplicate accounting is exercised.
///
/// [`MemEventSink::new`] only records. [`MemEventSink::folding_into`] also folds, as
/// `PgEventSink` does in the append's transaction: after each batch, every run it names that the
/// log's fold knows gets its `state`, `terminal`, `coordinator_job_id`, submitter, canceller,
/// experiment and name re-derived from `Projection::replay` of everything recorded, and its
/// `output_prefix`, `submitted_at`, `workflow`, `resumed_from` and launch time once the fold has
/// them (the row is created if the store lacks it). Only those fields are folded (a deliberate
/// simplification); the rest of a row is what the test seeded. Each control-plane event it records
/// is also readable through the store's `run_event` (as `PgEventSink` appends to the log
/// `PgStore::run_event` reads). So a folding sink's runs must be seeded from events (their anchor),
/// not only by `insert`.
///
/// Builds fold separately, into a [`MemRegistry`] ([`MemEventSink::fold_builds_into`]), as
/// `PgEventSink` re-folds `build_projection`: after each batch, every build subject it names gets
/// its projection replaced by `fold_builds` of everything recorded. Other subjects keep what the
/// test seeded.
#[derive(Default)]
pub struct MemEventSink {
    pub events: Mutex<Vec<RunEvent>>,
    /// The store the recorded events fold into (`None`: record only).
    folds_into: Option<Arc<MemStore>>,
    /// The registry recorded build events fold into (`None`: builds are not folded).
    builds_into: Mutex<Option<Arc<MemRegistry>>>,
    /// With a store: for each appended event, its kind and whether its run's launch was claimed
    /// in that store when it was recorded.
    pub recorded_under_claim: Mutex<Vec<(String, bool)>>,
}

impl MemEventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// A sink that folds what it records into `store` (see the type's doc).
    pub fn folding_into(store: Arc<MemStore>) -> Self {
        Self { folds_into: Some(store), ..Self::default() }
    }

    /// From now on, also fold recorded build events into `registry.builds` (see the type's doc).
    pub fn fold_builds_into(&self, registry: Arc<MemRegistry>) {
        *self.builds_into.lock().unwrap() = Some(registry);
    }

    /// The fold of everything recorded, as the real sink's projection would hold it.
    pub fn folded(&self) -> biffix_core::reducer::Projection {
        biffix_core::reducer::Projection::replay(&self.events.lock().unwrap())
    }

    /// The folded state of `run` (`None`: the log names no such run).
    pub fn folded_state(&self, run: &str) -> Option<biffix_core::state::RunState> {
        self.folded().runs.get(run).map(biffix_core::reducer::RunProjection::state)
    }
}

#[async_trait]
impl EventSink for MemEventSink {
    async fn record_all(&self, evs: &[RunEvent]) -> Result<usize, StoreError> {
        // One lock for the batch: all land together, as in the transactional store.
        let mut events = self.events.lock().unwrap();
        let mut appended = 0;
        for ev in evs {
            if !events.iter().any(|existing| existing.dedup_key == ev.dedup_key) {
                if let Some(store) = &self.folds_into {
                    let claimed = ev
                        .run_id
                        .as_ref()
                        .is_some_and(|r| store.launch_claims.lock().unwrap().contains(r.as_str()));
                    self.recorded_under_claim
                        .lock()
                        .unwrap()
                        .push((ev.kind().as_str().to_string(), claimed));
                    if ev.run_id.is_some() {
                        store.insert_anchor(ev.clone());
                    }
                }
                events.push(ev.clone());
                appended += 1;
            }
        }
        if let Some(store) = &self.folds_into {
            let fold = biffix_core::reducer::Projection::replay(&events);
            let mut runs = store.runs.lock().unwrap();
            // Each run the batch names, once, in the order it first names it.
            let mut seen = std::collections::HashSet::new();
            for run_id in evs.iter().filter_map(|e| e.run_id.as_ref()).filter(|r| seen.insert(*r)) {
                let Some(folded) = fold.runs.get(run_id.as_str()) else {
                    continue;
                };
                let state = folded.state();
                let row = runs
                    .entry(run_id.to_string())
                    .or_insert_with(|| run_detail(run_id.as_str(), state.as_str()));
                row.run.state = state.as_str().to_string();
                row.run.terminal = state.is_terminal();
                row.coordinator_job_id =
                    folded.coordinator_job_id.as_deref().map(CoordinatorJobId::new);
                // What the anchor and the launch record, once the log says.
                if folded.output_prefix.is_some() {
                    row.output_prefix.clone_from(&folded.output_prefix);
                }
                if let Some(at) = folded.submitted_at_micros {
                    row.run.submitted_at = Some(at as f64 / 1e6);
                }
                if let Some(version) = &folded.workflow_version_id {
                    row.run.workflow = Some(version.clone().into());
                }
                if let Some(prior) = &folded.resumed_from {
                    row.resumed_from = Some(RunId::new(prior.as_str()));
                }
                if folded.launched_at_micros.is_some() {
                    row.launched_at_micros = folded.launched_at_micros;
                }
                // Who submitted it, once the log says (a test may have seeded it before).
                if folded.submitter.is_some() {
                    row.run.submitter.clone_from(&folded.submitter);
                }
                row.cancelled_by = folded.cancelled_by();
                row.run.experiment_id = folded.experiment_id();
                // Its name, once the log names it (a launch name, or a rename that may clear it).
                let renamed = events.iter().any(|e| {
                    e.run_id.as_ref() == Some(run_id)
                        && matches!(e.data, biffix_core::event::EventData::RunRenamed { .. })
                });
                if renamed || folded.run_name.is_some() {
                    row.run.run_name.clone_from(&folded.run_name);
                }
                store.sync_notification(run_id.as_str(), folded.notification());
            }
        }
        if let Some(registry) = self.builds_into.lock().unwrap().as_ref() {
            let mut folded = biffix_core::builds::fold_builds(&events);
            let mut builds = registry.builds.lock().unwrap();
            for subject in evs.iter().filter_map(biffix_core::builds::subject_of) {
                if let Some(projection) = folded.remove(subject) {
                    builds.insert(subject.to_string(), projection);
                }
            }
        }
        Ok(appended)
    }

    async fn job_claims(
        &self,
        job_ids: &[biffix_core::ids::BatchJobId],
    ) -> Result<Vec<(String, RunId)>, StoreError> {
        // Faithful to the Postgres query: the distinct (job, run) bindings among these jobs.
        let events = self.events.lock().unwrap();
        let mut claims: Vec<(String, RunId)> = Vec::new();
        for e in events.iter().filter(|e| {
            matches!(
                e.kind(),
                biffix_core::event::Kind::TaskSubmitted
                    | biffix_core::event::Kind::CoordinatorSubmitted
            )
        }) {
            let (Some(job), Some(run)) = (&e.batch_job_id, &e.run_id) else {
                continue;
            };
            if job_ids.contains(job) {
                let claim = (job.as_str().to_string(), run.clone());
                if !claims.contains(&claim) {
                    claims.push(claim);
                }
            }
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::event_log::EventLogReads;
    use crate::store::outputs::OutputStore;

    #[tokio::test]
    async fn a_folded_submit_reads_as_postgres_reads_it() {
        let store = Arc::new(MemStore::new());
        let sink = MemEventSink::folding_into(store.clone());
        let run = RunId::new("r1");
        let mut anchor = biffix_core::event::RunSubmitted::new(
            "abc:hash:0",
            "abc",
            "sha256:d",
            serde_json::json!({}),
            vec![],
        );
        anchor.output_prefix = Some("s3://b/out/".into());
        let ev = biffix_core::event::run_submitted(
            &run,
            biffix_core::time::Timestamp::from_epoch_seconds(5.0),
            anchor,
        );
        sink.record_all(std::slice::from_ref(&ev)).await.unwrap();
        // The anchor is readable, as from the log.
        assert_eq!(store.run_anchor(&run).await.unwrap(), Some(ev));
        // The still-running run holds its prefix, so a resume or submit into it is refused.
        assert_eq!(store.active_run_in_prefix("s3://b/out/").await.unwrap(), Some(run.clone()));
        let row = store.runs.lock().unwrap()["r1"].clone();
        assert_eq!(row.run.submitted_at, Some(5.0));
        assert_eq!(
            row.run.workflow.as_ref().map(|w| w.as_str().to_string()),
            Some("abc:hash:0".to_string())
        );
    }
}
