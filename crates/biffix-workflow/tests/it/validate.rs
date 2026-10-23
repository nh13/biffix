//! Param-validation cases the shared table (`xtask/src/cases/validate.rs`, which the UI's port
//! replays) and the module's own tests leave out: a non-object parameter set, a number against a
//! string allow-list, the boolean spellings, coercing boolean strings, and the Nextflow `outdir`
//! default.
//! The required, enum-choice and input-path cases live in those two places, not here; a property
//! over required and enum-choice verdicts is in `properties.rs`.

use biffix_workflow::formschema::{FormField, WidgetHint};
use biffix_workflow::validate::{coerce_params, validate_params};

/// A plain (non-path) field with an optional allow-list.
fn spec(name: &str, required: bool, enum_values: Option<Vec<String>>) -> FormField {
    FormField {
        name: name.into(),
        widget: WidgetHint::Text,
        required,
        enum_values,
        ..Default::default()
    }
}

/// `validate_params` as `(field, message)` pairs.
fn errors(specs: &[FormField], params: &serde_json::Value) -> Vec<(String, String)> {
    validate_params(specs, params).into_iter().map(|e| (e.field, e.message)).collect()
}

fn pair(field: &str, message: &str) -> (String, String) {
    (field.to_string(), message.to_string())
}

#[test]
fn enum_rejects_non_string_value() {
    // A number for a string allow-list must be rejected, not silently accepted.
    let specs = vec![spec("mode", true, Some(vec!["fast".into()]))];
    assert_eq!(
        errors(&specs, &serde_json::json!({ "mode": 3 })),
        vec![pair("mode", "not one of fast")]
    );
}

#[test]
fn non_object_parameters_are_rejected() {
    assert_eq!(
        errors(&[], &serde_json::json!("raw-secret-like-scalar")),
        vec![pair("$", "parameters must be a JSON object")]
    );
}

// ── boolean params (a JSON-Schema boolean is a two-choice true/false Enum) ────────────────────────

fn boolean(name: &str) -> FormField {
    spec(name, false, Some(vec!["true".into(), "false".into()]))
}

#[test]
fn boolean_accepts_a_json_boolean_or_its_string_spelling() {
    let specs = vec![boolean("skip_qc")];
    for ok in [
        serde_json::json!({ "skip_qc": true }),
        serde_json::json!({ "skip_qc": false }),
        serde_json::json!({ "skip_qc": "true" }),
        serde_json::json!({ "skip_qc": "false" }),
    ] {
        assert!(errors(&specs, &ok).is_empty(), "{ok}");
    }
    for bad in [serde_json::json!({ "skip_qc": "yes" }), serde_json::json!({ "skip_qc": 1 })] {
        assert_eq!(errors(&specs, &bad), vec![pair("skip_qc", "must be true or false")], "{bad}");
    }
}

#[test]
fn coerce_turns_boolean_strings_into_booleans_and_leaves_the_rest() {
    let specs = vec![
        boolean("skip_qc"),
        boolean("trim"),
        spec("mode", false, Some(vec!["fast".into(), "slow".into()])),
    ];
    let got = coerce_params(
        &specs,
        serde_json::json!({
            "skip_qc": "false", "trim": true, "mode": "fast", "other": "true"
        }),
    );
    // Only declared booleans change; a string enum and an undeclared param keep their strings.
    assert_eq!(
        got,
        serde_json::json!({ "skip_qc": false, "trim": true, "mode": "fast", "other": "true" })
    );
    // An invalid spelling is left for the validator to reject, not silently dropped.
    assert_eq!(
        coerce_params(&specs, serde_json::json!({ "skip_qc": "yes" })),
        serde_json::json!({ "skip_qc": "yes" })
    );
}

// ── the Nextflow `outdir` default ────────────────────────────────────────────────────────────────

#[test]
fn an_unset_nextflow_outdir_defaults_under_the_prefix() {
    use biffix_workflow::validate::default_nextflow_outdir;
    let fields = vec![
        FormField { widget: WidgetHint::S3Dir, ..spec("outdir", true, None) },
        FormField { widget: WidgetHint::S3File, ..spec("reads", true, None) },
    ];
    let apply = |fields: &[FormField], mut params: serde_json::Value, prefix: Option<&str>| {
        default_nextflow_outdir(fields, &mut params, prefix);
        params
    };
    // A blank value counts as unset; a prefix without a trailing slash still joins cleanly.
    assert_eq!(
        apply(&fields, serde_json::json!({"outdir": " "}), Some("s3://b/root"))["outdir"],
        "s3://b/root/results"
    );
    assert_eq!(
        apply(&fields, serde_json::Value::Null, Some("s3://b/p/"))["outdir"],
        "s3://b/p/results"
    );
    // A given value is kept.
    assert_eq!(
        apply(&fields, serde_json::json!({"outdir": "s3://b/mine/"}), Some("s3://b/p/"))["outdir"],
        "s3://b/mine/"
    );
    // Undeclared, or nowhere to put it: untouched.
    assert_eq!(
        apply(&fields[1..], serde_json::json!({}), Some("s3://b/p/")),
        serde_json::json!({})
    );
    assert_eq!(apply(&fields, serde_json::json!({}), None), serde_json::json!({}));
}
