//! [`MemExperimentStore`]: the in-memory [`ExperimentStore`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::store::experiments::{
    name_conflict, ExperimentChange, ExperimentRecord, ExperimentStore,
};
use crate::store::StoreError;

/// It keeps the Postgres store's rules: a live name is unique per project, ignoring case.
#[derive(Default)]
pub struct MemExperimentStore {
    experiments: Mutex<BTreeMap<String, ExperimentRecord>>,
}

/// Whether `candidate` would share its name with another live experiment of its project in `all`,
/// ignoring case as `str::to_lowercase` folds it: exactly Postgres `lower()` (the deployed index's
/// rule) for ASCII names; see `pg::experiments::is_name_clash` for beyond ASCII.
fn clashes(all: &BTreeMap<String, ExperimentRecord>, candidate: &ExperimentRecord) -> bool {
    candidate.archived_at_micros.is_none()
        && all.values().any(|e| {
            e.id != candidate.id
                && e.archived_at_micros.is_none()
                && e.project == candidate.project
                && e.name.to_lowercase() == candidate.name.to_lowercase()
        })
}

#[async_trait]
impl ExperimentStore for MemExperimentStore {
    async fn list(
        &self,
        project: Option<&str>,
        include_archived: bool,
    ) -> Result<Vec<ExperimentRecord>, StoreError> {
        Ok(self
            .experiments
            .lock()
            .unwrap()
            .values()
            .filter(|e| project.is_none_or(|p| e.project == p))
            .filter(|e| include_archived || e.archived_at_micros.is_none())
            .cloned()
            .collect())
    }

    async fn get(&self, id: &str) -> Result<ExperimentRecord, StoreError> {
        self.experiments.lock().unwrap().get(id).cloned().ok_or(StoreError::NotFound)
    }

    async fn create(&self, record: &ExperimentRecord) -> Result<(), StoreError> {
        let mut all = self.experiments.lock().unwrap();
        if all.contains_key(&record.id) {
            return Err(StoreError::Conflict(format!("experiment {} already exists", record.id)));
        }
        if clashes(&all, record) {
            return Err(name_conflict(&record.project, &record.name));
        }
        all.insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn update(
        &self,
        id: &str,
        change: &ExperimentChange,
    ) -> Result<ExperimentRecord, StoreError> {
        let mut all = self.experiments.lock().unwrap();
        let mut next = all.get(id).cloned().ok_or(StoreError::NotFound)?;
        apply(&mut next, change);
        if clashes(&all, &next) {
            return Err(name_conflict(&next.project, &next.name));
        }
        all.insert(id.to_string(), next.clone());
        Ok(next)
    }
}

/// `change` applied to `record`, as the Postgres store's `UPDATE` applies it.
fn apply(record: &mut ExperimentRecord, change: &ExperimentChange) {
    if let Some(name) = &change.name {
        record.name.clone_from(name);
    }
    if let Some(description) = &change.description {
        record.description.clone_from(description);
    }
    match change.archived {
        Some(true) => {
            record.archived_at_micros = record.archived_at_micros.or(Some(change.now_micros));
        }
        Some(false) => record.archived_at_micros = None,
        None => {}
    }
}
