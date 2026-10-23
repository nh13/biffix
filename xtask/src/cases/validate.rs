//! A table of param-validation cases with their expected verdicts, shared across languages.
//!
//! The UI validates live with a TypeScript port of [`biffix_workflow::validate`]; the server stays
//! authoritative. To keep the two from drifting, these cases are asserted against the Rust
//! validator (`tests` below) and emitted to `schemas/validate-cases.json` (by
//! `cargo xtask emit-schemas`), which the UI's tests replay against its port.

use serde::Serialize;
use serde_json::{json, Value};

use biffix_workflow::formschema::{FormField, Preset, Rule, ShowIf, WidgetHint};

#[derive(Debug, Serialize)]
pub(crate) struct ValidateCase {
    name: &'static str,
    fields: Vec<FormField>,
    params: Value,
    /// Fields `inactive_fields` reports, sorted.
    inactive: Vec<String>,
    /// `(field, message)` pairs `validate_params` reports, in field order.
    errors: Vec<(String, String)>,
    /// What the UI's port reports instead, where it deliberately reports less: a `rules` pattern
    /// outside the subset JS and the `regex` crate read alike is left to the server. Always a
    /// subset of `errors` — the port may miss an error, never add one.
    #[serde(skip_serializing_if = "Option::is_none")]
    ui_errors: Option<Vec<(String, String)>>,
}

fn f(name: &str, widget: WidgetHint) -> FormField {
    FormField { name: name.into(), widget, ..FormField::default() }
}

fn boolean(name: &str) -> FormField {
    FormField {
        enum_values: Some(vec!["true".into(), "false".into()]),
        ..f(name, WidgetHint::Enum)
    }
}

fn when(field: &str, values: Vec<Value>) -> Vec<ShowIf> {
    vec![ShowIf { field: field.into(), values }]
}

fn errs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

/// The cases. Each exercises one rule of the validator or the active set.
pub(crate) fn cases() -> Vec<ValidateCase> {
    let umi = || {
        vec![
            FormField { default: Some(json!(false)), ..boolean("with_umi") },
            FormField {
                required: true,
                enum_values: Some(vec!["string".into(), "regex".into()]),
                show_if: when("with_umi", vec![json!(true)]),
                ..f("umi_method", WidgetHint::Enum)
            },
            FormField {
                required: true,
                show_if: when("umi_method", vec![json!("regex")]),
                ..f("umi_regex", WidgetHint::Text)
            },
        ]
    };
    let reads = || FormField {
        min: Some(1.0),
        max: Some(64.0),
        step: Some(1.0),
        ..f("threads", WidgetHint::Number)
    };
    vec![
        ValidateCase {
            name: "required field missing",
            fields: vec![FormField { required: true, ..f("reads", WidgetHint::Text) }],
            params: json!({}),
            inactive: vec![],
            errors: errs(&[("reads", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "null counts as missing",
            fields: vec![FormField { required: true, ..f("reads", WidgetHint::Text) }],
            params: json!({"reads": null}),
            inactive: vec![],
            errors: errs(&[("reads", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "gate off hides and relaxes required sub-fields",
            fields: umi(),
            params: json!({"umi_regex": "x"}),
            inactive: vec!["umi_method".into(), "umi_regex".into()],
            errors: vec![],
            ui_errors: None,
        },
        ValidateCase {
            name: "gate on requires the sub-field, transitively hiding its own sub-field",
            fields: umi(),
            params: json!({"with_umi": true}),
            inactive: vec!["umi_regex".into()],
            errors: errs(&[("umi_method", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "string spelling of a boolean opens the gate",
            fields: umi(),
            params: json!({"with_umi": "true", "umi_method": "regex"}),
            inactive: vec![],
            errors: errs(&[("umi_regex", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "gate uses the default when the param is absent",
            fields: vec![
                FormField {
                    default: Some(json!("fast")),
                    enum_values: Some(vec!["fast".into(), "slow".into()]),
                    ..f("mode", WidgetHint::Enum)
                },
                FormField {
                    show_if: when("mode", vec![json!("slow")]),
                    ..f("depth", WidgetHint::Number)
                },
            ],
            params: json!({}),
            inactive: vec!["depth".into()],
            errors: vec![],
            ui_errors: None,
        },
        ValidateCase {
            name: "number range and integer checks",
            fields: vec![reads()],
            params: json!({"threads": 100}),
            inactive: vec![],
            errors: errs(&[("threads", "must be between 1 and 64")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "numeric string is a number",
            fields: vec![reads()],
            params: json!({"threads": "8"}),
            inactive: vec![],
            errors: vec![],
            ui_errors: None,
        },
        ValidateCase {
            name: "fraction where a whole number is required",
            fields: vec![reads()],
            params: json!({"threads": 2.5}),
            inactive: vec![],
            errors: errs(&[("threads", "must be a whole number")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "whole numbers follow the param's type, not its step hint",
            fields: vec![
                FormField {
                    json_type: Some("integer".into()),
                    step: Some(5.0),
                    ..f("depth", WidgetHint::Number)
                },
                FormField {
                    json_type: Some("number".into()),
                    step: Some(1.0),
                    ..f("ratio", WidgetHint::Number)
                },
            ],
            params: json!({"depth": 7.5, "ratio": 2.5}),
            inactive: vec![],
            errors: errs(&[("depth", "must be a whole number")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a blank string does not satisfy required",
            fields: vec![
                FormField { required: true, ..f("reads", WidgetHint::Text) },
                FormField {
                    required: true,
                    default: Some(json!("hg38")),
                    ..f("genome", WidgetHint::Text)
                },
                f("note", WidgetHint::Text),
            ],
            params: json!({"reads": " \t", "genome": "", "note": ""}),
            inactive: vec![],
            errors: errs(&[("reads", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "text in a number field",
            fields: vec![reads()],
            params: json!({"threads": "many"}),
            inactive: vec![],
            errors: errs(&[("threads", "must be a number")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "only a minimum",
            fields: vec![FormField { min: Some(0.5), ..f("frac", WidgetHint::Number) }],
            params: json!({"frac": 0.25}),
            inactive: vec![],
            errors: errs(&[("frac", "must be at least 0.5")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "pattern rule",
            fields: vec![FormField {
                rules: vec![Rule {
                    pattern: r"^[ACGT]+$".into(),
                    message: "must be a DNA sequence".into(),
                }],
                ..f("adapter", WidgetHint::Text)
            }],
            params: json!({"adapter": "ACGN"}),
            inactive: vec![],
            errors: errs(&[("adapter", "must be a DNA sequence")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "enum and boolean",
            fields: vec![
                FormField {
                    enum_values: Some(vec!["a".into(), "b".into()]),
                    ..f("choice", WidgetHint::Enum)
                },
                boolean("flag"),
            ],
            params: json!({"choice": "c", "flag": "yes"}),
            inactive: vec![],
            errors: errs(&[("choice", "not one of a, b"), ("flag", "must be true or false")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a numeric choice matches by value, however it is spelled",
            fields: ["two", "two_text", "two_float", "five", "listed"]
                .into_iter()
                .map(|name| FormField {
                    enum_values: Some(vec!["1".into(), "2".into(), "4".into()]),
                    json_type: Some("integer".into()),
                    ..f(name, WidgetHint::Enum)
                })
                .collect(),
            params: json!({
                "two": 2, "two_text": "2", "two_float": 2.0, "five": 5, "listed": [2],
            }),
            inactive: vec![],
            errors: errs(&[("five", "not one of 1, 2, 4"), ("listed", "not one of 1, 2, 4")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a string param with true/false choices is not a boolean",
            fields: vec![
                FormField { json_type: Some("string".into()), ..boolean("word") },
                FormField { json_type: Some("boolean".into()), ..boolean("flag") },
                // A form frozen before `json_type` was recorded: the choices' shape decides.
                boolean("legacy"),
            ],
            params: json!({"word": "yes", "flag": "yes", "legacy": "yes"}),
            inactive: vec![],
            errors: errs(&[
                ("word", "not one of true, false"),
                ("flag", "must be true or false"),
                ("legacy", "must be true or false"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "path input must be a string",
            fields: vec![
                FormField { is_path_input: true, ..f("fasta", WidgetHint::S3File) },
                FormField { is_path_input: true, ..f("listed", WidgetHint::S3File) },
            ],
            params: json!({"fasta": 3, "listed": ["s3://b/a.fa"]}),
            inactive: vec![],
            errors: errs(&[
                ("fasta", "must be a string path"),
                ("listed", "must be a string path"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a multiple path input takes a list of paths, each checked",
            fields: ["reads", "one", "bad_suffix", "empty", "mixed", "scalar"]
                .into_iter()
                .map(|name| FormField {
                    is_path_input: true,
                    multiple: true,
                    rules: vec![Rule {
                        pattern: r"\.fq\.gz$".into(),
                        message: "must be a .fq.gz file".into(),
                    }],
                    ..f(name, WidgetHint::S3File)
                })
                .collect(),
            params: json!({
                "reads": ["s3://b/a.fq.gz", "s3://b/b.fq.gz"],
                "one": "s3://b/a.fq.gz",
                "bad_suffix": ["s3://b/a.fq.gz", "s3://b/b.txt"],
                "empty": [],
                "mixed": ["s3://b/a.fq.gz", 3],
                "scalar": 3,
            }),
            inactive: vec![],
            errors: errs(&[
                ("bad_suffix", "path 2: must be a .fq.gz file"),
                ("empty", "must list at least one path"),
                ("mixed", "must be a list of string paths"),
                ("scalar", "must be a string path"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a pattern checks a number or a boolean as its text, never a list",
            fields: ["short", "long", "listed", "flag"]
                .into_iter()
                .map(|name| ruled(name, r"^\d{3}$"))
                .collect(),
            params: json!({"short": 123, "long": 12345, "listed": ["123"], "flag": true}),
            inactive: vec![],
            errors: errs(&[
                ("long", "must match"),
                ("listed", "must be text"),
                ("flag", "must match"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a default satisfies required",
            fields: vec![
                FormField {
                    required: true,
                    default: Some(json!("hg38")),
                    ..f("genome", WidgetHint::Text)
                },
                FormField { required: true, default: Some(json!(false)), ..boolean("skip_qc") },
                FormField { required: true, ..f("reads", WidgetHint::Text) },
            ],
            params: json!({"skip_qc": null}),
            inactive: vec![],
            errors: errs(&[("reads", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a show_if cycle is always shown",
            fields: vec![
                FormField { show_if: when("b", vec![json!("x")]), ..f("a", WidgetHint::Text) },
                FormField { show_if: when("a", vec![json!("y")]), ..f("b", WidgetHint::Text) },
                // Gated on a cyclic field: judged by its condition as usual.
                FormField {
                    required: true,
                    show_if: when("a", vec![json!("1")]),
                    ..f("on", WidgetHint::Text)
                },
                FormField {
                    required: true,
                    show_if: when("a", vec![json!("nope")]),
                    ..f("off", WidgetHint::Text)
                },
            ],
            params: json!({"a": "1", "b": "2"}),
            inactive: vec!["off".into()],
            errors: errs(&[("on", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a show_if cycle is shown in any field order",
            fields: vec![
                FormField { show_if: when("a", vec![json!("y")]), ..f("b", WidgetHint::Text) },
                FormField { show_if: when("c", vec![json!("z")]), ..f("a", WidgetHint::Text) },
                FormField {
                    required: true,
                    show_if: when("b", vec![json!("x")]),
                    ..f("c", WidgetHint::Text)
                },
            ],
            params: json!({"a": "y", "b": "z"}),
            inactive: vec![],
            errors: errs(&[("c", "required")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "only a maximum, and a step other than 1 is not enforced",
            fields: vec![
                FormField { max: Some(10.0), ..f("cap", WidgetHint::Number) },
                FormField { step: Some(0.5), ..f("half", WidgetHint::Number) },
            ],
            params: json!({"cap": 11, "half": 0.3}),
            inactive: vec![],
            errors: errs(&[("cap", "must be at most 10")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "exclusive bounds and multipleOf",
            fields: vec![
                FormField {
                    exclusive_min: Some(0.0),
                    exclusive_max: Some(1.0),
                    ..f("fraction", WidgetHint::Number)
                },
                FormField { exclusive_min: Some(0.0), ..f("positive", WidgetHint::Number) },
                FormField { multiple_of: Some(0.1), ..f("tenths", WidgetHint::Number) },
                FormField { multiple_of: Some(4.0), ..f("by_four", WidgetHint::Number) },
            ],
            params: json!({"fraction": 1, "positive": 0, "tenths": 0.3, "by_four": 6}),
            inactive: vec![],
            errors: errs(&[
                ("fraction", "must be less than 1"),
                ("positive", "must be greater than 0"),
                ("by_four", "must be a multiple of 4"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a number that is not finite is not a number",
            fields: vec![
                FormField { min: Some(1.0), max: Some(64.0), ..f("bounded", WidgetHint::Number) },
                FormField { min: Some(1.0), ..f("min_only", WidgetHint::Number) },
                FormField { max: Some(1.0), ..f("max_only", WidgetHint::Number) },
                f("unbounded", WidgetHint::Number),
            ],
            params: json!({
                "bounded": "NaN", "min_only": "inf", "max_only": "-Infinity", "unbounded": "nan",
            }),
            inactive: vec![],
            errors: errs(&[
                ("bounded", "must be a number"),
                ("min_only", "must be a number"),
                ("max_only", "must be a number"),
                ("unbounded", "must be a number"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a preset value validates as the field",
            fields: vec![FormField {
                presets: vec![Preset { label: "Duplex".into(), value: json!(3) }],
                min: Some(1.0),
                ..f("min_reads", WidgetHint::Number)
            }],
            params: json!({"min_reads": 3}),
            inactive: vec![],
            errors: vec![],
            ui_errors: None,
        },
        ValidateCase {
            name: "number spellings the server reads",
            fields: ["hex", "exponent", "infinite", "fraction", "separated"]
                .into_iter()
                .map(|name| FormField {
                    min: Some(1.0),
                    max: Some(64.0),
                    step: Some(1.0),
                    ..f(name, WidgetHint::Number)
                })
                .collect(),
            params: json!({
                "hex": "0x10", "exponent": "1e1", "infinite": "inf", "fraction": ".5",
                "separated": "1_0",
            }),
            inactive: vec![],
            errors: errs(&[
                ("hex", "must be a number"),
                ("infinite", "must be a number"),
                ("fraction", "must be a whole number"),
                ("separated", "must be a number"),
            ]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a Unicode category reads alike in both regex dialects",
            fields: vec![ruled("word", r"^\p{L}+$"), ruled("not_word", r"^\p{L}+$")],
            params: json!({"word": "abc", "not_word": "ab1"}),
            inactive: vec![],
            errors: errs(&[("not_word", "must match")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "regex-crate-only syntax is left to the server",
            fields: vec![
                ruled("anchored", r"\A[ACGT]+\z"),
                ruled("not_anchored", r"\A[ACGT]+\z"),
                ruled("flagged", r"(?i)^[acgt]+$"),
                ruled("not_flagged", r"(?i)^[acgt]+$"),
                ruled("posix", r"^[[:upper:]]+$"),
                ruled("not_posix", r"^[[:upper:]]+$"),
                ruled("intersected", r"^[a-z&&[^aeiou]]+$"),
                ruled("not_intersected", r"^[a-z&&[^aeiou]]+$"),
            ],
            params: json!({
                "anchored": "ACGT", "not_anchored": "ACGN",
                "flagged": "ACGT", "not_flagged": "hello",
                "posix": "ACGT", "not_posix": "acgt",
                "intersected": "xyz", "not_intersected": "abc",
            }),
            inactive: vec![],
            errors: errs(&[
                ("not_anchored", "must match"),
                ("not_flagged", "must match"),
                ("not_posix", "must match"),
                ("not_intersected", "must match"),
            ]),
            ui_errors: Some(vec![]),
        },
        ValidateCase {
            name: "text outside ASCII is left to the server",
            fields: vec![
                ruled("digits", r"^\d+$"),
                ruled("not_digits", r"^\d+$"),
                ruled("any", r"^a.b$"),
            ],
            params: json!({"digits": "٠١٢", "not_digits": "12x", "any": "a\rb"}),
            inactive: vec![],
            errors: errs(&[("not_digits", "must match")]),
            ui_errors: None,
        },
        ValidateCase {
            name: "a pattern the regex crate refuses is skipped",
            fields: vec![
                ruled("lookahead", r"^(?!tmp)"),
                ruled("backreference", r"^(a)\1$"),
                ruled("backspace", r"^[\b]$"),
                ruled("too_big", r"^\W{1,100}\W{1,100}\W{1,100}$"),
            ],
            params: json!({
                "lookahead": "tmpx", "backreference": "ab", "backspace": "x", "too_big": "abc",
            }),
            inactive: vec![],
            errors: vec![],
            ui_errors: None,
        },
    ]
}

/// A text field with one `rules` pattern, whose message is `must match`.
fn ruled(name: &str, pattern: &str) -> FormField {
    FormField {
        rules: vec![Rule { pattern: pattern.into(), message: "must match".into() }],
        ..f(name, WidgetHint::Text)
    }
}

/// The cases as the JSON the UI replays.
pub(crate) fn cases_json() -> String {
    crate::artifacts::pretty_json(&cases())
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_workflow::validate::{inactive_fields, validate_params};

    #[test]
    fn every_case_holds_for_the_rust_validator() {
        for case in cases() {
            let inactive: Vec<String> =
                inactive_fields(&case.fields, &case.params).into_iter().collect();
            assert_eq!(inactive, case.inactive, "inactive: {}", case.name);
            let errors: Vec<(String, String)> = validate_params(&case.fields, &case.params)
                .into_iter()
                .map(|e| (e.field, e.message))
                .collect();
            assert_eq!(errors, case.errors, "errors: {}", case.name);
            // The port may leave an error to the server, never report one the server wouldn't.
            if let Some(ui) = &case.ui_errors {
                assert!(
                    ui.iter().all(|e| case.errors.contains(e)),
                    "ui_errors not a subset of errors: {}",
                    case.name
                );
            }
        }
    }
}
