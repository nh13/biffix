use serde_json::json;

use super::*;
use crate::manifest::BiffixManifest;
use crate::samples::{ColumnRole, SheetFormat};

const ROW_SCHEMA: &str = r#"{"items": {"required": ["sample", "fastq_1"], "properties": {
        "sample": {"type": "string", "meta": ["id"]},
        "fastq_1": {"type": "string", "format": "file-path"},
        "fastq_2": {"type": "string", "format": "file-path"}}}}"#;

fn read(p: &str) -> Result<String, String> {
    match p {
        "assets/schema_input.json" => Ok(ROW_SCHEMA.to_string()),
        other => Err(format!("no {other}")),
    }
}

#[test]
fn nf_row_schemas_become_samplesheet_columns() {
    let schema = json!({"properties": {
        "input": {"type": "string", "format": "file-path", "schema": "assets/schema_input.json"},
        "other": {"type": "string", "format": "file-path", "schema": "assets/missing.json"}
    }, "required": ["input"]});
    let form = from_nextflow_schema_ordered(
        &schema,
        &SchemaOrder::default(),
        &BiffixManifest::default(),
        &read,
    );
    let spec = field(&form, "input").samplesheet.clone().unwrap();
    let names: Vec<&str> = spec.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["sample", "fastq_1", "fastq_2"]);
    assert_eq!(spec.columns[0].role, Some(ColumnRole::SampleId));
    assert!(field(&form, "other").samplesheet.is_none());
    assert_eq!(
        form.warnings,
        ["`other`: cannot read samplesheet schema assets/missing.json: no assets/missing.json"]
    );
}

#[test]
fn a_row_schema_comes_from_the_copy_of_a_param_that_won() {
    // `input` is in two groups; the first group's copy (no row schema) is the field, so the
    // second copy's `schema` must not be read into it.
    let schema = json!({"$defs": {
        "a": {"properties": {"input": {"type": "string", "format": "file-path"}}},
        "b": {"properties": {"input": {"type": "string", "schema": "assets/schema_input.json"}}}
    }});
    let form = from_nextflow_schema_ordered(
        &schema,
        &SchemaOrder::default(),
        &BiffixManifest::default(),
        &read,
    );
    assert_eq!(form.fields.len(), 1);
    assert_eq!(field(&form, "input").widget, WidgetHint::S3File);
    assert!(field(&form, "input").samplesheet.is_none());
}

#[test]
fn an_overlay_with_no_columns_leaves_the_row_schema_s_columns() {
    // "Ignored" means the derived spec stays, not that it is dropped too.
    let schema = json!({"properties": {
        "input": {"type": "string", "format": "file-path", "schema": "assets/schema_input.json"}
    }});
    let manifest = BiffixManifest::parse_yaml(
        "form:\n  params:\n    input:\n      samplesheet:\n        columns: []\n        format: tsv\n",
    )
    .unwrap();
    let form = from_nextflow_schema_ordered(&schema, &SchemaOrder::default(), &manifest, &read);
    let form = apply_overlay(form, manifest.form.as_ref().unwrap(), &|_| Err("x".into()));
    let spec = field(&form, "input").samplesheet.clone().unwrap();
    let names: Vec<&str> = spec.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["sample", "fastq_1", "fastq_2"]);
    assert_eq!(spec.format, SheetFormat::Csv, "nothing of the ignored overlay applies");
    assert_eq!(form.warnings, ["form.params.input: samplesheet declares no columns; ignored"]);
}

#[test]
fn a_samplesheet_overlay_on_a_param_that_is_not_one_is_ignored() {
    // Without `format: samplesheet` the field is plain text: the form would never show the
    // columns, so the API must not act on them either.
    let manifest = BiffixManifest::parse_yaml(
        "form:\n  params:\n    samples:\n      samplesheet:\n        columns:\n          - { name: sample, required: true }\n",
    )
    .unwrap();
    let form = sm_adapt(&json!({"properties": {"samples": {"type": "string"}}}), &manifest);
    let form = apply_overlay(form, manifest.form.as_ref().unwrap(), &|_| Err("x".into()));
    let f = field(&form, "samples");
    assert_eq!(f.widget, WidgetHint::Text);
    assert!(f.samplesheet.is_none());
    assert_eq!(
        form.warnings,
        ["form.params.samples: samplesheet applies only to a samplesheet param (add `format: \
             samplesheet`); ignored"]
    );
}

#[test]
fn the_overlay_declares_a_snakemake_samplesheet() {
    let manifest = BiffixManifest::parse_yaml(
        r#"
form:
  params:
    samples:
      format: samplesheet
      samplesheet:
        format: tsv
        paired_only: true
        columns:
          - { name: sample, required: true }
          - { name: r1, required: true, file: true, suffixes: [.fastq.gz] }
          - { name: r2, file: true }
          - { name: umi, enum_values: ["8M+T", "custom"], default: "8M+T" }
"#,
    )
    .unwrap();
    let form = sm_adapt(&json!({"properties": {"samples": {"type": "string"}}}), &manifest);
    let form = apply_overlay(form, manifest.form.as_ref().unwrap(), &|_| Err("x".into()));
    let f = field(&form, "samples");
    assert_eq!(f.widget, WidgetHint::Samplesheet);
    let spec = f.samplesheet.clone().unwrap();
    assert_eq!(spec.format, SheetFormat::Tsv);
    assert!(spec.paired_only);
    let roles: Vec<Option<ColumnRole>> = spec.columns.iter().map(|c| c.role).collect();
    assert_eq!(
        roles,
        [Some(ColumnRole::SampleId), Some(ColumnRole::Read1), Some(ColumnRole::Read2), None]
    );
    assert_eq!(spec.columns[1].suffixes, [".fastq.gz"]);
}
