use serde_json::json;

use super::*;

#[test]
fn kind_as_str_matches_serde() {
    // Every known kind's as_str must equal its serde wire tag (both directions), so SQL and logs
    // built from as_str cannot drift from what is stored.
    for kind in Kind::KNOWN {
        let wire = serde_json::to_value(kind).unwrap();
        assert_eq!(wire.as_str().unwrap(), kind.as_str(), "{kind:?}");
        let back: Kind = serde_json::from_value(wire).unwrap();
        assert_eq!(&back, kind);
        assert_eq!(&Kind::from_tag(kind.as_str()), kind);
    }
    // The open-enum escape hatch preserves its raw string.
    assert_eq!(Kind::Unknown("future_kind".into()).as_str(), "future_kind");
    assert_eq!(Kind::from_tag("future_kind"), Kind::Unknown("future_kind".into()));
}

/// One minimal payload per known kind, as a producer from before any optional field existed
/// would have stored it.
fn minimal_payloads() -> Vec<serde_json::Value> {
    vec![
        json!({"kind": "run_submitted", "workflow_version_id": "wf1", "git_sha": "abc",
               "image_digest": "", "params": {}, "input_uris": []}),
        json!({"kind": "run_started"}),
        json!({"kind": "run_progress", "completed": 1, "total": 2}),
        json!({"kind": "run_succeeded"}),
        json!({"kind": "run_failed"}),
        json!({"kind": "run_cancelled"}),
        json!({"kind": "cancel_requested", "actor": "u"}),
        json!({"kind": "work_deleted"}),
        json!({"kind": "run_preparing", "workflow_version_id": "wf1", "params": {},
               "input_uris": [], "images": []}),
        json!({"kind": "experiment_set"}),
        json!({"kind": "run_renamed"}),
        json!({"kind": "coordinator_submitted"}),
        json!({"kind": "coordinator_running"}),
        json!({"kind": "coordinator_succeeded"}),
        json!({"kind": "coordinator_failed"}),
        json!({"kind": "run_dag", "dag": {}}),
        json!({"kind": "task_created"}),
        json!({"kind": "task_submitted"}),
        json!({"kind": "task_queued"}),
        json!({"kind": "task_started"}),
        json!({"kind": "task_succeeded", "exit_code": 0}),
        json!({"kind": "task_cached"}),
        json!({"kind": "task_failed", "exit_code": 1}),
        json!({"kind": "task_retrying", "prev_attempt": 1, "next_attempt": 2}),
        json!({"kind": "engine_task_verdict", "verdict": "failed"}),
        json!({"kind": "spot_reclaim_warning"}),
        json!({"kind": "task_outputs", "outputs": []}),
        json!({"kind": "build_requested", "subject": "s", "build_arn": "a"}),
        json!({"kind": "build_started", "subject": "s", "build_arn": "a"}),
        json!({"kind": "build_succeeded", "subject": "s", "build_arn": "a"}),
        json!({"kind": "build_failed", "subject": "s", "build_arn": "a", "reason": "FAILED"}),
    ]
}

#[test]
fn every_known_kind_parses_from_a_minimal_legacy_payload() {
    // A stored known-kind payload that no longer matches its variant silently degrades to
    // Unknown, which the fold skips. Pin one minimal (legacy) payload per kind: adding a
    // required field to a kind, or a kind without a payload here, fails this test.
    let payloads = minimal_payloads();
    let tags: Vec<&str> = payloads.iter().map(|p| p["kind"].as_str().unwrap()).collect();
    let known: Vec<&str> = Kind::KNOWN.iter().map(Kind::as_str).collect();
    assert_eq!(tags, known, "one minimal payload per known kind, in order");
    for payload in payloads {
        let data: EventData = serde_json::from_value(payload.clone()).unwrap();
        assert!(!data.is_degraded_known_kind(), "{payload} degraded");
        assert_eq!(data.kind().as_str(), payload["kind"].as_str().unwrap());
    }
}

#[test]
fn a_known_kind_with_a_malformed_payload_is_flagged_degraded() {
    let degraded: EventData =
        serde_json::from_value(json!({"kind": "run_progress", "completed": 1})).unwrap();
    assert!(matches!(degraded, EventData::Unknown(_)));
    assert!(degraded.is_degraded_known_kind());

    // A genuinely unknown kind is not degraded, and neither is a translator's unknown payload.
    let future: EventData = serde_json::from_value(json!({"kind": "future_kind", "x": 1})).unwrap();
    assert!(!future.is_degraded_known_kind());
    assert_eq!(future.kind(), Kind::Unknown("future_kind".into()));
    let wrapped = EventData::unknown("aws_batch_new_state", json!({"status": "NEW_STATE"}));
    assert!(!wrapped.is_degraded_known_kind());
    assert_eq!(wrapped.kind().as_str(), "aws_batch_new_state");
    assert!(!EventData::RunSucceeded {}.is_degraded_known_kind());
}

#[test]
fn stored_events_without_session_fields_still_parse_as_their_kind() {
    // Rows stored before resume support must fold as their known kind, not degrade to Unknown.
    let started: EventData =
        serde_json::from_value(json!({"kind": "run_started"})).expect("bare run_started parses");
    assert_eq!(started, EventData::RunStarted { session_id: None });

    let anchor: EventData = serde_json::from_value(json!({
        "kind": "run_submitted", "workflow_version_id": "wf1", "git_sha": "abc",
        "image_digest": "", "params": {}, "input_uris": []
    }))
    .expect("legacy anchor parses");
    let EventData::RunSubmitted(anchor) = anchor else {
        panic!("legacy anchor degraded to {anchor:?}");
    };
    assert_eq!((anchor.engine_session_id, anchor.resumed_from), (None, None));

    // And the unset fields stay off the wire.
    let wire = serde_json::to_value(EventData::RunStarted { session_id: None }).unwrap();
    assert_eq!(wire, json!({"kind": "run_started"}));
}

#[test]
fn run_failed_reads_with_or_without_a_reason() {
    let bare: EventData = serde_json::from_value(json!({"kind": "run_failed"})).unwrap();
    assert_eq!(bare, EventData::RunFailed { reason: None, stage: None });
    let why: EventData = serde_json::from_value(
        json!({"kind": "run_failed", "reason": "no image", "stage": "some_newer_stage"}),
    )
    .unwrap();
    assert_eq!(
        why,
        EventData::RunFailed {
            reason: Some("no image".into()),
            stage: Some("some_newer_stage".into())
        }
    );
    // The engine's reason-less terminal keeps its wire shape.
    let wire = serde_json::to_value(EventData::RunFailed { reason: None, stage: None }).unwrap();
    assert_eq!(wire, json!({"kind": "run_failed"}));

    // The control plane's failure is its own fact, never deduped against the engine's.
    let ev = run_failed(
        &crate::ids::RunId::new("r1"),
        Timestamp::from_epoch_seconds(1.0),
        FAILURE_STAGE_IMAGE_PREPARATION,
        "no image",
    );
    assert_eq!(
        serde_json::to_value(&ev.data).unwrap(),
        json!({"kind": "run_failed", "reason": "no image", "stage": "image_preparation"})
    );
    assert_eq!(ev.source, EventSource::BiffixControlPlane);
    assert_eq!(ev.dedup_key, "biffix-control-plane:r1:::run_failed");
}

#[test]
fn an_unknown_retention_does_not_degrade_the_anchor() {
    // A newer backend's retention value, read by this build: the anchor still parses as the
    // anchor, the value is preserved verbatim (the sweep acts on it as the default).
    let stored = json!({
        "kind": "run_submitted", "workflow_version_id": "wf1", "git_sha": "abc",
        "image_digest": "", "params": {}, "input_uris": [], "retention": "archive"
    });
    let data: EventData = serde_json::from_value(stored.clone()).unwrap();
    let EventData::RunSubmitted(anchor) = &data else {
        panic!("anchor degraded to {data:?}");
    };
    let retention = anchor.retention.clone().unwrap();
    assert_eq!(retention, Retention::Unknown("archive".into()));
    assert_eq!(retention.as_str(), "archive");
    assert_eq!(serde_json::to_value(&data).unwrap(), stored, "round-trips verbatim");

    for known in [Retention::Default, Retention::DeleteOnSuccess, Retention::Keep] {
        let wire = serde_json::to_value(&known).unwrap();
        assert_eq!(wire.as_str().unwrap(), known.as_str());
        assert_eq!(known.as_str().parse::<Retention>().unwrap(), known);
    }
    assert_eq!("archive".parse::<Retention>().unwrap(), Retention::Unknown("archive".into()));
}

#[test]
fn the_boxed_anchor_keeps_the_flat_wire_shape() {
    // Boxing the anchor is an in-memory change only: the stored JSON is the same flat object.
    let anchor = RunSubmitted {
        submitter: Some("user:u".into()),
        retention: Some(Retention::Keep),
        ..RunSubmitted::new("wf1", "abc", "sha256:x", json!({"k": "v"}), vec!["s3://b/in".into()])
    };
    let ev =
        run_submitted(&crate::ids::RunId::new("r1"), Timestamp::from_epoch_seconds(1.0), anchor);
    let wire = serde_json::to_value(&ev).unwrap();
    assert_eq!(
        wire,
        json!({
            "schema_version": SCHEMA_VERSION,
            "dedup_key": "biffix-control-plane:r1:::run_submitted",
            "ts": 1.0,
            "source": "biffix-control-plane",
            "run_id": "r1",
            "kind": "run_submitted",
            "workflow_version_id": "wf1",
            "git_sha": "abc",
            "image_digest": "sha256:x",
            "params": {"k": "v"},
            "input_uris": ["s3://b/in"],
            "submitter": "user:u",
            "retention": "keep"
        })
    );
    let back: RunEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
    assert_eq!(back, ev);
    // The anchor no longer sets the size of every event.
    assert!(
        std::mem::size_of::<EventData>() <= 128,
        "EventData is {} bytes",
        std::mem::size_of::<EventData>()
    );
}

#[test]
fn the_runevent_schema_admits_unknown_kinds() {
    // The derived schema modelled the untagged fallback as `kind == "unknown"`, rejecting the
    // very unknown kinds the contract preserves. The published branch accepts any kind string
    // other than the known ones (so the oneOf stays exclusive).
    let schema = serde_json::to_value(schemars::schema_for!(RunEvent)).unwrap();
    let branches = schema["oneOf"].as_array().expect("RunEvent data is a oneOf");
    assert!(
        branches.iter().all(|b| b["properties"]["kind"]["const"] != json!("unknown")
            && b["properties"]["kind"]["enum"] != json!(["unknown"])),
        "no branch may pin kind to \"unknown\""
    );
    let unknown: Vec<&serde_json::Value> =
        branches.iter().filter(|b| b["properties"]["kind"].get("not").is_some()).collect();
    assert_eq!(unknown.len(), 1, "exactly one open fallback branch");
    let kind = &unknown[0]["properties"]["kind"];
    assert_eq!(kind["type"], "string");
    let excluded: Vec<&str> =
        kind["not"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let known: Vec<&str> = Kind::KNOWN.iter().map(Kind::as_str).collect();
    assert_eq!(excluded, known);
    assert_eq!(branches.len(), known.len() + 1, "one branch per known kind + the fallback");
}

#[test]
fn terminal_kind_constants_are_terminals() {
    // Guards the shared constants the reconcile SQL derives from.
    assert_eq!(
        TASK_TERMINAL_KINDS.map(|k| k.as_str().to_string()),
        ["task_succeeded", "task_failed"]
    );
    assert_eq!(
        COORDINATOR_TERMINAL_KINDS.map(|k| k.as_str().to_string()),
        ["coordinator_succeeded", "coordinator_failed"]
    );
}

/// Extract the documentation `examples` from an open-enum's generated JSON Schema.
fn schema_examples<T: schemars::JsonSchema>() -> Vec<String> {
    let mut generator = schemars::SchemaGenerator::default();
    let schema = T::json_schema(&mut generator);
    assert_eq!(
        schema.get("type"),
        Some(&json!("string")),
        "an open-enum schema must be a plain string schema"
    );
    schema
        .get("examples")
        .and_then(serde_json::Value::as_array)
        .expect("open-enum schema carries examples")
        .iter()
        .map(|v| v.as_str().expect("example is a string").to_string())
        .collect()
}

#[test]
fn kind_schema_examples_are_the_known_kinds() {
    let known: Vec<String> = Kind::KNOWN.iter().map(|k| k.as_str().to_string()).collect();
    assert_eq!(schema_examples::<Kind>(), known);
}

#[test]
fn a_verdict_this_build_does_not_know_does_not_degrade_the_event() {
    let newer: EventData = serde_json::from_value(
        json!({"kind": "engine_task_verdict", "verdict": "warned", "reason": "slow"}),
    )
    .unwrap();
    assert_eq!(
        newer,
        EventData::EngineTaskVerdict {
            verdict: TaskVerdict::Unknown("warned".into()),
            category: None,
            reason: Some("slow".into()),
        }
    );
    assert_eq!(TaskVerdict::Unknown("warned".into()).as_str(), "warned");
    assert_eq!(
        serde_json::to_value(TaskVerdict::Failed).unwrap(),
        json!(TaskVerdict::Failed.as_str())
    );
    assert_eq!(schema_examples::<TaskVerdict>(), ["failed"]);
    // A stored task_submitted without an image still reads as the kind.
    let bare: EventData = serde_json::from_value(json!({"kind": "task_submitted"})).unwrap();
    assert_eq!(bare, EventData::TaskSubmitted { image: None });
    assert_eq!(serde_json::to_value(&bare).unwrap(), json!({"kind": "task_submitted"}));
}

#[test]
fn termination_category_as_str_is_its_serde_tag() {
    for c in [
        TerminationCategory::Spot,
        TerminationCategory::Oom,
        TerminationCategory::HostTerminated,
        TerminationCategory::AppError,
        TerminationCategory::MissingOutput,
        TerminationCategory::Unknown("other".into()),
    ] {
        assert_eq!(serde_json::to_value(&c).unwrap(), json!(c.as_str()));
    }
}

#[test]
fn termination_category_schema_examples_match_variants() {
    let variant_tags: Vec<String> = [
        TerminationCategory::Spot,
        TerminationCategory::Oom,
        TerminationCategory::HostTerminated,
        TerminationCategory::AppError,
        TerminationCategory::MissingOutput,
    ]
    .iter()
    .map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_string())
    .collect();
    assert_eq!(schema_examples::<TerminationCategory>(), variant_tags);
}
