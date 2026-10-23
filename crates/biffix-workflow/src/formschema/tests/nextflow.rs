use serde_json::json;

use super::*;
use crate::manifest::BiffixManifest;

#[test]
fn adapts_grouped_nf_schema() {
    let schema = json!({
        "definitions": {
            "input_output_options": {
                "type": "object",
                "required": ["input", "outdir"],
                "properties": {
                    "input": {
                        "type": "string",
                        "format": "file-path",
                        "schema": "assets/schema_input.json",
                        "description": "Path to samplesheet"
                    },
                    "outdir": { "type": "string", "format": "directory-path", "description": "Output dir" },
                    "fasta": { "type": "string", "format": "file-path", "help_text": "Reference FASTA" }
                }
            },
            "generic_options": {
                "type": "object",
                "properties": {
                    "genome": { "type": "string", "enum": ["GRCh37", "GRCh38"], "description": "Genome" },
                    "cpus": { "type": "integer", "default": 4 },
                    "trim": { "type": "boolean", "description": "Trim reads" }
                }
            }
        }
    });
    let fs = nf_adapt(&schema, &BiffixManifest::default());
    // The test reads no repo files, so the row schema is the only warning.
    assert_eq!(
        fs.warnings,
        ["`input`: cannot read samplesheet schema assets/schema_input.json: no assets/schema_input.json"]
    );
    assert_eq!(fs.fields.len(), 6);

    // Samplesheet: the `input` param carries its own row schema.
    let input = field(&fs, "input");
    assert_eq!(input.widget, WidgetHint::Samplesheet);
    assert!(input.required && input.is_path_input && !input.is_output);

    // outdir is an output location, not a HEAD-checked input.
    let outdir = field(&fs, "outdir");
    assert_eq!(outdir.widget, WidgetHint::S3Dir);
    assert!(outdir.is_output && !outdir.is_path_input);

    // A plain input file is HEAD-checked; help_text becomes the description.
    let fasta = field(&fs, "fasta");
    assert_eq!(fasta.widget, WidgetHint::S3File);
    assert!(fasta.is_path_input && !fasta.required);
    assert_eq!(fasta.description.as_deref(), Some("Reference FASTA"));

    // enum → choice; integer → number; boolean → two-value choice.
    assert_eq!(field(&fs, "genome").widget, WidgetHint::Enum);
    assert_eq!(
        field(&fs, "genome").enum_values.as_deref(),
        Some(&["GRCh37".to_string(), "GRCh38".to_string()][..])
    );
    assert_eq!(field(&fs, "cpus").widget, WidgetHint::Number);
    let trim = field(&fs, "trim");
    assert_eq!(trim.widget, WidgetHint::Enum);
    assert_eq!(trim.enum_values.as_deref(), Some(&["true".to_string(), "false".to_string()][..]));
}

#[test]
fn adapts_v2_defs_and_top_level_props() {
    // nf-schema v2 uses `$defs`; also exercise ungrouped top-level properties.
    let schema = json!({
        "$defs": {
            "opts": { "properties": { "aligner": { "type": "string" } } }
        },
        "properties": { "email": { "type": "string", "description": "Notify" } }
    });
    let fs = nf_adapt(&schema, &BiffixManifest::default());
    assert_eq!(fs.fields.len(), 2);
    assert_eq!(field(&fs, "aligner").widget, WidgetHint::Text);
    assert_eq!(field(&fs, "email").widget, WidgetHint::Text);
}

#[test]
fn empty_schema_warns_not_panics() {
    let fs = nf_adapt(&json!({"title": "x"}), &BiffixManifest::default());
    assert!(fs.fields.is_empty());
    assert_eq!(fs.warnings.len(), 1);
}
