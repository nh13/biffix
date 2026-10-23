//! The formschema tests, one file per area, and the helpers they share.

use super::*;
use crate::manifest::BiffixManifest;

mod attach_catalog;
mod attach_reports;
mod attach_samples;
mod help_and_unit;
mod hint_and_keyword;
mod nextflow;
mod samplesheet;
mod snakemake;
mod suffix;
mod v2;

/// A manifest parsed from `yaml`.
fn manifest(yaml: &str) -> BiffixManifest {
    BiffixManifest::parse_yaml(yaml).unwrap()
}

/// The file reader of a workflow with no repo files.
fn no_files(path: &str) -> Result<String, String> {
    Err(format!("no {path}"))
}

/// Adapt an nf-schema built with `json!` (whose maps sort, so the order is alphabetical) with no
/// repo files.
fn nf_adapt(schema: &Value, manifest: &BiffixManifest) -> FormSchema {
    from_nextflow_schema_ordered(schema, &SchemaOrder::default(), manifest, &no_files)
}

/// Adapt a Snakemake config schema built with `json!` (alphabetical order).
fn sm_adapt(schema: &Value, manifest: &BiffixManifest) -> FormSchema {
    from_snakemake_config_ordered(schema, &SchemaOrder::default(), manifest)
}

/// Look up an adapted field by name (shared by both adapters' test modules).
fn field<'a>(fs: &'a FormSchema, name: &str) -> &'a FormField {
    fs.fields.iter().find(|f| f.name == name).expect("field present")
}
