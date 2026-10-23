//! Renaming a run (`POST /runs/{id}/name`) over the in-memory doubles: the rename is recorded as the
//! run's `run_renamed` event, folds into the name every read serves, and a refold of the whole log
//! agrees; a blank or `null` name clears it; a body without `run_name`, a name too long, or an
//! unknown run is refused. The route's capability gate is covered by `capability_gates.rs`.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use biffix_backend::testing::{self, get_json, post_json, MemEventSink, MemStore, TestState};
use biffix_backend::{api, AppState};
use biffix_core::event::EventData;
use biffix_core::ids::RunId;

struct Harness {
    events: Arc<MemEventSink>,
    state: AppState,
}

/// A run `r1` launched (by its anchor) with the name `launch`.
async fn harness() -> Harness {
    let store = Arc::new(MemStore::new());
    let events = Arc::new(MemEventSink::folding_into(store.clone()));
    let state = TestState::new().with_store(store.clone()).with_events(events.clone()).build();
    let mut anchor =
        biffix_core::event::RunSubmitted::new("v1:h:0", "v1", "sha256:1a2b", json!({}), vec![]);
    anchor.run_name = Some("launch".into());
    let ev = biffix_core::event::run_submitted(
        &RunId::new("r1"),
        biffix_core::time::Timestamp::from_epoch_seconds(1.0),
        anchor,
    );
    use biffix_backend::store::EventSink as _;
    events.record(&ev).await.unwrap();
    Harness { events, state }
}

impl Harness {
    fn app(&self) -> axum::Router {
        api::router(self.state.clone())
    }

    async fn rename(&self, run: &str, body: Value) -> (StatusCode, Value) {
        post_json(self.app(), &format!("/runs/{run}/name"), body).await
    }

    /// The name `GET /runs/{run}` serves, which must be what a refold of the whole log says.
    async fn served_name(&self, run: &str) -> Option<String> {
        let (status, v) = get_json(self.app(), &format!("/runs/{run}")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let served = v.get("run_name").and_then(Value::as_str).map(str::to_string);
        assert_eq!(served, self.events.folded().runs[run].run_name, "{v}");
        served
    }

    fn renames(&self) -> Vec<Option<String>> {
        self.events
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match &e.data {
                EventData::RunRenamed { run_name, actor } => {
                    assert_eq!(actor.as_deref(), Some("local"));
                    Some(run_name.clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[tokio::test]
async fn a_rename_is_an_event_that_folds_into_the_served_name() {
    let h = harness().await;
    assert_eq!(h.served_name("r1").await.as_deref(), Some("launch"));

    // Trimmed, recorded, and answered with the run's summary after.
    let (status, v) = h.rename("r1", json!({"run_name": "  Liver batch 2 "})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["run_name"], "Liver batch 2");
    assert_eq!(h.served_name("r1").await.as_deref(), Some("Liver batch 2"));

    // Blank and null both clear it: readers derive a name, the launch name does not come back.
    let (status, v) = h.rename("r1", json!({"run_name": "   "})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("run_name").is_none(), "{v}");
    assert_eq!(h.served_name("r1").await, None);
    h.rename("r1", json!({"run_name": "Again"})).await;
    let (status, _) = h.rename("r1", json!({"run_name": null})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.served_name("r1").await, None);

    assert_eq!(h.renames(), [Some("Liver batch 2".to_string()), None, Some("Again".into()), None]);
}

#[tokio::test]
async fn a_bad_rename_is_refused_and_records_nothing() {
    let h = harness().await;
    // `api/runs.rs` RUN_RENAME_BODY_ERROR, which the UI and the mock repeat.
    const BODY_ERROR: &str = "the body must be JSON naming `run_name`: a name, or null to clear it";
    // No `run_name` (a misspelled field is not read as "clear"), and not JSON at all.
    let (status, v) = h.rename("r1", json!({"name": "x"})).await;
    assert_eq!((status, v.as_str()), (StatusCode::BAD_REQUEST, Some(BODY_ERROR)));
    let (status, v) = h.rename("r1", json!({})).await;
    assert_eq!((status, v.as_str()), (StatusCode::BAD_REQUEST, Some(BODY_ERROR)));
    let raw = |content_type: Option<&str>| {
        let mut req = axum::http::Request::post("/runs/r1/name");
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        req.body(axum::body::Body::from("not json")).unwrap()
    };
    let (status, v) = testing::send(h.app(), raw(Some("application/json"))).await;
    assert_eq!((status, v.as_str()), (StatusCode::BAD_REQUEST, Some(BODY_ERROR)));
    // No JSON content type: axum's own refusal, not the body message.
    let (status, _) = testing::send(h.app(), raw(None)).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // Too long: the submit limit (200 characters) holds for a rename too.
    let (status, v) = h.rename("r1", json!({"run_name": "x".repeat(201)})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(v.as_str().is_some_and(|m| m.starts_with("run_name must be at most")), "{v}");
    let (status, _) = h.rename("r1", json!({"run_name": "é".repeat(200)})).await;
    assert_eq!(status, StatusCode::OK);
    // An unknown run.
    let (status, _) = h.rename("nope", json!({"run_name": "x"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(h.renames(), [Some("é".repeat(200))]);
}
