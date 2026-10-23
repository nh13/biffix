//! Experiments: named groups of a project's runs. An experiment is biffix-domain state, like a
//! workflow version: rows written by the API (the `experiments` table), not a fold. Which
//! experiment a run is in is a fact about the run, recorded as its `experiment_set` event (or the
//! anchor's `experiment_id`) and folded into the run projection. An experiment grants nothing:
//! viewing one needs `ViewRuns`, changing one (or its runs) `LaunchWorkflow`.

use crate::store::experiments::ExperimentRecord;

use crate::error::ServiceError;
use crate::store::StoreError;
use crate::AppState;

/// The longest experiment name, in characters.
pub(crate) const MAX_NAME_CHARS: usize = 200;
/// The longest experiment description, in characters.
pub(crate) const MAX_DESCRIPTION_CHARS: usize = 4000;

/// A new experiment id: opaque, like a run's (`exp-<uuid>`).
pub(crate) fn new_id() -> String {
    format!("exp-{}", uuid::Uuid::new_v4())
}

/// A name as stored: trimmed, 1..=[`MAX_NAME_CHARS`] characters. The error says which bound it
/// missed (blank, or too long and by how much).
pub(crate) fn normalize_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("an experiment name must not be blank".to_string());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!(
            "an experiment name is {chars} characters; the limit is {MAX_NAME_CHARS}"
        ));
    }
    Ok(name.to_string())
}

/// A description as stored: trimmed, blank ⇒ none, at most [`MAX_DESCRIPTION_CHARS`] characters.
pub(crate) fn normalize_description(description: Option<&str>) -> Result<Option<String>, String> {
    let Some(d) = description.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    let chars = d.chars().count();
    if chars > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "an experiment description is {chars} characters; the limit is {MAX_DESCRIPTION_CHARS}"
        ));
    }
    Ok(Some(d.to_string()))
}

/// The experiment a run may be put in: it exists, is in the run's `project`, and is not archived.
/// An unknown one is a 404 when `unknown_is_404` (it is the request's subject), else a 400 (a field
/// of a request about something else).
pub(crate) async fn joinable_experiment(
    state: &AppState,
    id: &str,
    project: &str,
    unknown_is_404: bool,
) -> Result<ExperimentRecord, ServiceError> {
    let record = match state.experiments.get(id).await {
        Ok(record) => record,
        Err(StoreError::NotFound) => {
            let message = format!("unknown experiment {id}");
            return Err(if unknown_is_404 {
                ServiceError::NotFound(message)
            } else {
                ServiceError::Invalid(message)
            });
        }
        Err(e) => return Err(ServiceError::from(e)),
    };
    if project != record.project {
        return Err(ServiceError::Invalid(format!(
            "experiment {id} is in project {}, not the run's ({project})",
            record.project
        )));
    }
    if record.archived_at_micros.is_some() {
        return Err(ServiceError::Invalid(format!(
            "experiment {id} is archived; restore it to add runs"
        )));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::experiments::{ExperimentChange, ExperimentStore};
    use crate::store::runs::RunStats;
    use crate::testing::MemExperimentStore;

    fn record(id: &str, project: &str, name: &str) -> ExperimentRecord {
        ExperimentRecord {
            id: id.into(),
            project: project.into(),
            name: name.into(),
            description: None,
            created_by: None,
            created_by_label: None,
            created_at_micros: 1,
            archived_at_micros: None,
        }
    }

    #[test]
    fn activity_is_the_later_of_the_newest_run_and_the_creation() {
        let mut e = record("e1", "p", "A");
        e.created_at_micros = 100;
        let stats = |last| RunStats { count: 1, last_submitted_micros: last };
        assert_eq!(e.activity_micros(None), 100);
        assert_eq!(e.activity_micros(Some(&stats(None))), 100);
        assert_eq!(e.activity_micros(Some(&stats(Some(50)))), 100);
        assert_eq!(e.activity_micros(Some(&stats(Some(150)))), 150);
    }

    #[test]
    fn names_and_descriptions_are_trimmed_and_bounded() {
        assert_eq!(normalize_name("  RNA batch 1 ").unwrap(), "RNA batch 1");
        assert_eq!(normalize_name("   ").unwrap_err(), "an experiment name must not be blank");
        assert_eq!(
            normalize_name(&"x".repeat(MAX_NAME_CHARS + 1)).unwrap_err(),
            "an experiment name is 201 characters; the limit is 200"
        );
        // Characters, not bytes: 200 four-byte characters fit.
        assert!(normalize_name(&"🧪".repeat(MAX_NAME_CHARS)).is_ok());
        assert_eq!(normalize_description(Some("  ")).unwrap(), None);
        assert_eq!(normalize_description(None).unwrap(), None);
        assert_eq!(normalize_description(Some(" why ")).unwrap().as_deref(), Some("why"));
        assert_eq!(
            normalize_description(Some(&"x".repeat(MAX_DESCRIPTION_CHARS + 1))).unwrap_err(),
            "an experiment description is 4001 characters; the limit is 4000"
        );
    }

    #[tokio::test]
    async fn a_live_name_is_unique_per_project_ignoring_case_and_archiving_frees_it() {
        let store = MemExperimentStore::default();
        store.create(&record("e1", "p", "Batch")).await.unwrap();
        let clash = store.create(&record("e2", "p", "batch")).await.unwrap_err();
        assert!(matches!(clash, StoreError::Conflict(m) if m.contains("`batch`")));
        // Another project may use it.
        store.create(&record("e3", "q", "batch")).await.unwrap();
        // Archived, it gives the name up; restoring it then clashes.
        let archive =
            ExperimentChange { archived: Some(true), now_micros: 5, ..Default::default() };
        assert_eq!(store.update("e1", &archive).await.unwrap().archived_at_micros, Some(5));
        store.create(&record("e2", "p", "batch")).await.unwrap();
        let restore = ExperimentChange { archived: Some(false), ..Default::default() };
        assert!(matches!(store.update("e1", &restore).await, Err(StoreError::Conflict(_))));
        // Renamed, it can come back.
        let rename = ExperimentChange {
            name: Some("Batch (old)".into()),
            archived: Some(false),
            ..Default::default()
        };
        let back = store.update("e1", &rename).await.unwrap();
        assert_eq!((back.name.as_str(), back.archived_at_micros), ("Batch (old)", None));
        assert!(matches!(store.update("nope", &rename).await, Err(StoreError::NotFound)));
    }
}
