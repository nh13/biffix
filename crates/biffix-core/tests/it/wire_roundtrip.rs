//! Wire-contract behavior tests: float `ts` round-trip and open-enum unknown preservation.

use biffix_core::event;
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use biffix_core::wire::aws_batch::{AwsBatchEvent, BatchStatus};
use biffix_core::wire::aws_ec2::{Ec2Event, Ec2EventKind};

#[test]
fn timestamp_roundtrips_through_float_seconds() {
    let ts = Timestamp::from_epoch_seconds(1776142351.5);
    let json = serde_json::to_string(&ts).unwrap();
    assert_eq!(json, "1776142351.5");
    let back: Timestamp = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ts);
    assert_eq!(back.as_micros(), 1_776_142_351_500_000);
}

#[test]
fn unknown_source_is_preserved_not_dropped() {
    // The bug this pins: the schema modelled Unknown as an object, so a bare-string unknown was
    // rejected. It must parse to Unknown and re-serialize as the same bare string.
    let v: EventSource = serde_json::from_str("\"some-future-adapter\"").unwrap();
    assert_eq!(v, EventSource::Unknown("some-future-adapter".into()));
    assert_eq!(serde_json::to_string(&v).unwrap(), "\"some-future-adapter\"");
}

#[test]
fn known_source_parses_kebab_case() {
    let v: EventSource = serde_json::from_str("\"aws-batch\"").unwrap();
    assert_eq!(v, EventSource::AwsBatch);
}

#[test]
fn aws_batch_wire_event_parses_from_its_snake_case_shape() {
    let json = r#"{
        "ts": 1776142351.0,
        "batch_job_id": "abc123",
        "status": "FAILED",
        "exit_code": 137,
        "status_reason": "OutOfMemoryError",
        "instance_id": "i-0999",
        "is_coordinator": false
    }"#;
    let ev: AwsBatchEvent = serde_json::from_str(json).unwrap();
    assert_eq!(ev.status, BatchStatus::Failed);
    assert_eq!(ev.exit_code, Some(137));
    assert_eq!(ev.batch_job_id.as_str(), "abc123");
}

#[test]
fn ec2_spot_warning_parses() {
    let json =
        r#"{"ts": 1776142351.0, "instance_id": "i-0999", "kind": "spot_interruption_warning"}"#;
    let ev: Ec2Event = serde_json::from_str(json).unwrap();
    assert_eq!(ev.kind, Ec2EventKind::SpotInterruptionWarning);
}

#[test]
fn unknown_batch_status_is_preserved() {
    let v: BatchStatus = serde_json::from_str("\"SOME_NEW_STATUS\"").unwrap();
    assert_eq!(v, BatchStatus::Unknown("SOME_NEW_STATUS".into()));
    assert_eq!(serde_json::to_string(&v).unwrap(), "\"SOME_NEW_STATUS\"");
}

// ── open-enum schema: unknown values are bare strings, schema is `type: string` ──

#[test]
fn open_enum_schema_is_a_plain_string() {
    // A closed list (`enum`, or a `oneOf`/`anyOf` of constants) would make a validator reject the
    // unknown bare strings the open enum exists to accept.
    let schemas = [
        ("Source", schemars::schema_for!(EventSource)),
        ("Kind", schemars::schema_for!(biffix_core::event::Kind)),
        ("Retention", schemars::schema_for!(event::Retention)),
        ("StepScope", schemars::schema_for!(event::StepScope)),
        ("TerminationCategory", schemars::schema_for!(event::TerminationCategory)),
        ("TaskVerdict", schemars::schema_for!(event::TaskVerdict)),
        ("OutputRole", schemars::schema_for!(event::OutputRole)),
    ];
    for (name, schema) in schemas {
        let v = serde_json::to_value(&schema).unwrap();
        assert_eq!(v["type"], "string", "{name}: open enum must be a string schema");
        for closed in ["enum", "const", "oneOf", "anyOf", "allOf"] {
            assert!(v.get(closed).is_none(), "{name}: `{closed}` closes the open enum: {v}");
        }
    }
}

/// A bare unknown string parses to `T`'s `Unknown` (`unknown`) and serializes back as itself.
fn round_trips_unknown<T>(unknown: T)
where
    T: serde::de::DeserializeOwned + serde::Serialize + PartialEq + std::fmt::Debug,
{
    let parsed: T = serde_json::from_str("\"future_value\"").unwrap();
    assert_eq!(parsed, unknown);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), "\"future_value\"");
}

#[test]
fn unknown_open_enum_values_round_trip_as_bare_strings() {
    let unknown = || "future_value".to_string();
    // Every open enum (`BatchStatus` and `EventSource` also have their own tests above).
    round_trips_unknown(BatchStatus::Unknown(unknown()));
    round_trips_unknown(EventSource::Unknown(unknown()));
    round_trips_unknown(event::Kind::Unknown(unknown()));
    round_trips_unknown(Ec2EventKind::Unknown(unknown()));
    round_trips_unknown(biffix_core::wire::aws_codebuild::CodeBuildStatus::Unknown(unknown()));
    round_trips_unknown(event::Retention::Unknown(unknown()));
    round_trips_unknown(event::StepScope::Unknown(unknown()));
    round_trips_unknown(event::TerminationCategory::Unknown(unknown()));
    round_trips_unknown(event::TaskVerdict::Unknown(unknown()));
    round_trips_unknown(event::OutputRole::Unknown(unknown()));
}
