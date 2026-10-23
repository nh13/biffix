use serde_json::json;

use super::*;
use crate::manifest::BiffixManifest;

#[test]
fn a_non_string_enum_keeps_its_choices() {
    let fs = nf_adapt(
        &json!({"properties": {
            "k": {"type": "integer", "enum": [1, 2]},
            "b": {"type": "boolean", "enum": [true, false]},
            "n": {"type": ["string", "null"], "enum": ["a", null]},
            "e": {"type": "string", "enum": []}
        }}),
        &BiffixManifest::default(),
    );
    assert_eq!(field(&fs, "k").widget, WidgetHint::Enum);
    assert_eq!(
        field(&fs, "k").enum_values.as_deref(),
        Some(&["1".to_string(), "2".to_string()][..])
    );
    assert_eq!(
        field(&fs, "b").enum_values.as_deref(),
        Some(&["true".to_string(), "false".to_string()][..])
    );
    // A nullable enum offers its values; `null` is "not given".
    assert_eq!(field(&fs, "n").enum_values.as_deref(), Some(&["a".to_string()][..]));
    // An empty enum is no enum, with a warning.
    assert_eq!(field(&fs, "e").widget, WidgetHint::Text);
    assert_eq!(field(&fs, "e").enum_values, None);
    assert_eq!(fs.warnings, ["param `e`: enum lists no choices; any value is accepted"]);
}

#[test]
fn a_nullable_type_reads_as_its_non_null_type() {
    let fs = sm_adapt(
        &json!({"properties": {"n": {"type": ["null", "integer"]}}}),
        &BiffixManifest::default(),
    );
    assert_eq!(field(&fs, "n").widget, WidgetHint::Number);
    assert_eq!(field(&fs, "n").step, Some(1.0));
    assert_eq!(field(&fs, "n").json_type.as_deref(), Some("integer"));
}

#[test]
fn exclusive_bounds_and_multiple_of_are_carried() {
    let fs = sm_adapt(
        &json!({"properties": {
            "a": {"type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1, "multipleOf": 0.25},
            "b": {"type": "number", "minimum": 2, "exclusiveMinimum": true, "maximum": 5},
            "c": {"type": "integer", "multipleOf": 0}
        }}),
        &BiffixManifest::default(),
    );
    let a = field(&fs, "a");
    assert_eq!(
        (a.min, a.max, a.exclusive_min, a.exclusive_max, a.multiple_of),
        (None, None, Some(0.0), Some(1.0), Some(0.25))
    );
    // Draft 4: `exclusiveMinimum: true` makes `minimum` exclusive.
    let b = field(&fs, "b");
    assert_eq!(
        (b.min, b.max, b.exclusive_min, b.exclusive_max),
        (None, Some(5.0), Some(2.0), None)
    );
    assert_eq!(field(&fs, "c").multiple_of, None);
    assert_eq!(fs.warnings, ["param `c`: multipleOf must be greater than 0; ignored"]);
}

#[test]
fn a_pattern_the_server_cannot_check_is_dropped_with_a_warning() {
    let schema = json!({"properties": {
        "twice": {"type": "string", "pattern": "^(a)\\1$"},
        "fine": {"type": "string", "pattern": "^[a-z]+$"}
    }});
    let m = manifest(
        "form:\n  params:\n    fine:\n      rules:\n        - { pattern: '^(?!tmp)' }\n        - { pattern: '^x' }\n",
    );
    let form = nf_adapt(&schema, &m);
    assert!(field(&form, "twice").rules.is_empty());
    let form = apply_overlay(form, m.form.as_ref().unwrap(), &|_| Err("x".into()));
    let patterns: Vec<&str> =
        field(&form, "fine").rules.iter().map(|r| r.pattern.as_str()).collect();
    assert_eq!(patterns, ["^[a-z]+$", "^x"]);
    assert_eq!(form.warnings.len(), 2, "{:?}", form.warnings);
    assert!(form.warnings[0].starts_with(
        "param `twice`: pattern `^(a)\\1$` cannot be checked by the server (backreferences"
    ));
    assert!(form.warnings[1].starts_with(
        "form.params.fine: pattern `^(?!tmp)` cannot be checked by the server (look-around"
    ));
    assert!(form.warnings.iter().all(|w| w.ends_with("); dropped")));
}

#[test]
fn hints_it_cannot_honor_are_warnings() {
    let m = manifest(
        "params:\n  token: { format: secret }\n  reads: { format: s3file }\n  n: { output: true, multiple: true }\n",
    );
    let fs = sm_adapt(
        &json!({"properties": {
            "token": {"type": "string"},
            "reads": {"type": "string"},
            "n": {"type": "integer"}
        }}),
        &m,
    );
    // `format: secret` is honored as a secret.
    assert!(field(&fs, "token").is_secret);
    assert_eq!(field(&fs, "reads").widget, WidgetHint::Text);
    assert!(!field(&fs, "n").is_output);
    // Ignored means not frozen: a `multiple` number would be sent as a list.
    assert!(!field(&fs, "n").multiple);
    assert_eq!(
        fs.warnings,
        [
            "params.n: `output: true` applies only to a file or folder path; ignored",
            "params.n: `multiple: true` applies only to a file or folder path; ignored",
            "params.reads: format `s3file` is not one of s3-file, s3-dir, reference-bundle, samplesheet; ignored",
            "params.token: `format: secret` is spelled `secret: true`; treated as a secret",
        ]
    );
}

#[test]
fn output_is_honored_on_a_schema_path() {
    let m = manifest("params:\n  out_bam: { output: true }\n  results: { output: true }\n");
    let fs = sm_adapt(
        &json!({"properties": {
            "out_bam": {"type": "string", "format": "file-path"},
            "results": {"type": "string", "format": "directory-path"},
            "refs": {"type": "string", "format": "directory-path"}
        }}),
        &m,
    );
    for name in ["out_bam", "results"] {
        let f = field(&fs, name);
        assert!(f.is_output && !f.is_path_input, "{name}");
    }
    let refs = field(&fs, "refs");
    assert!(!refs.is_output && refs.is_path_input);
    assert!(fs.warnings.is_empty(), "{:?}", fs.warnings);
}

#[test]
fn outdir_is_an_output_by_convention_only_for_nextflow() {
    let schema = json!({"properties": {"outdir": {"type": "string", "format": "directory-path"}}});
    let nf = nf_adapt(&schema, &BiffixManifest::default());
    assert!(field(&nf, "outdir").is_output);
    assert!(nf.warnings.is_empty(), "{:?}", nf.warnings);
    let sm = sm_adapt(&schema, &BiffixManifest::default());
    assert!(field(&sm, "outdir").is_path_input && !field(&sm, "outdir").is_output);
    assert_eq!(
        sm.warnings,
        ["param `outdir` is checked as an input folder; add `output: true` under `params.outdir` in biffix.yaml if the workflow writes to it"]
    );
}

#[test]
fn an_nf_schema_path_accepts_a_file_or_a_folder() {
    let fs = nf_adapt(
        &json!({"properties": {
            "p": {"type": "string", "format": "path"},
            "f": {"type": "string", "format": "file-path"}
        }}),
        &BiffixManifest::default(),
    );
    let p = field(&fs, "p");
    assert_eq!(p.widget, WidgetHint::S3File);
    assert!(p.is_path_input && p.accepts_folder);
    assert!(!field(&fs, "f").accepts_folder);
}

#[test]
fn the_multiple_hint_reaches_the_field() {
    let m = manifest(
        "params:\n  input_fastqs: { format: s3-file, suffix: .fastq.gz, multiple: true }\n",
    );
    let fs = sm_adapt(
        &json!({"properties": {"input_fastqs": {"type": "string"}, "one": {"type": "string", "format": "file-path"}}}),
        &m,
    );
    assert!(field(&fs, "input_fastqs").multiple);
    assert!(!field(&fs, "one").multiple);
    let json = serde_json::to_value(field(&fs, "one")).unwrap();
    assert!(json.get("multiple").is_none(), "false is not serialized");
}

#[test]
fn the_overlay_default_is_a_prefill_not_the_engine_default() {
    let m = manifest(
        "form:\n  params:\n    with_umi: { default: true }\n    aligner: { default: bwa }\n    note: { min: 1, step: 2 }\n    reads: { min: 5, max: 1 }\n",
    );
    let schema = json!({"properties": {
        "with_umi": {"type": "boolean", "default": false},
        "aligner": {"type": "string", "enum": ["star", "salmon"], "default": "star"},
        "note": {"type": "string"},
        "reads": {"type": "integer"}
    }});
    let form = apply_overlay(sm_adapt(&schema, &m), m.form.as_ref().unwrap(), &|_| Err("x".into()));
    let umi = field(&form, "with_umi");
    assert_eq!((umi.default.clone(), umi.prefill.clone()), (Some(json!(false)), Some(json!(true))));
    // A prefill outside the choices is dropped; the engine default stands.
    let aligner = field(&form, "aligner");
    assert_eq!((aligner.default.clone(), aligner.prefill.clone()), (Some(json!("star")), None));
    assert_eq!((field(&form, "note").min, field(&form, "note").step), (None, None));
    assert_eq!(
        form.warnings,
        [
            "form.params.aligner: default `bwa` is not one of the choices; ignored",
            "form.params.note: `min` applies only to a number; ignored",
            "form.params.note: `step` applies only to a number; ignored",
            "form.params.reads: min 5 is above max 1; no value can be given",
        ]
    );
}

#[test]
fn overlay_values_json_cannot_hold_are_warnings() {
    let m = manifest(
        "form:\n  params:\n    x:\n      presets: { A: !tag 3, B: 4 }\n      show_if: { 1: yes, y: !tag z }\n",
    );
    let schema = json!({"properties": {
        "x": {"type": "integer"}, "1": {"type": "string"}, "y": {"type": "string"}
    }});
    let form = apply_overlay(sm_adapt(&schema, &m), m.form.as_ref().unwrap(), &|_| Err("x".into()));
    let x = field(&form, "x");
    assert_eq!(x.presets, [Preset { label: "B".into(), value: json!(4) }]);
    // A numeric key names the param `1`.
    assert_eq!(x.show_if, [ShowIf { field: "1".into(), values: vec![json!("yes")] }]);
    assert_eq!(
        form.warnings,
        [
            "form.params.x: preset `A` is not a plain value; ignored",
            "form.params.x: show_if value for `y` is not a plain value; ignored",
        ]
    );
}
