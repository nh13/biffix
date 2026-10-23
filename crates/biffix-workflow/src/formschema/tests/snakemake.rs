use serde_json::json;

use super::*;
use crate::manifest::BiffixManifest;

#[test]
fn snakemake_config_uses_the_shared_picker() {
    // Exercises the folded picker: the hint ladder, enum, number, boolean→two-value, and
    // the asymmetry fix — a config param carrying a JSON-Schema `format: file-path` now gets S3
    // path detection + is HEAD-checked, like Nextflow (it used to fall back to plain Text).
    let schema = json!({
        "required": ["ref"],
        "properties": {
            "genome": { "type": "string", "enum": ["GRCh37", "GRCh38"] },
            "threads": { "type": "integer" },
            "trim": { "type": "boolean", "description": "Trim reads" },
            "ref": { "type": "string", "format": "file-path" },
            "note": { "type": "string" }
        }
    });
    let fs = sm_adapt(&schema, &BiffixManifest::default());
    assert!(fs.warnings.is_empty(), "warnings: {:?}", fs.warnings);

    assert_eq!(field(&fs, "genome").widget, WidgetHint::Enum);
    assert_eq!(field(&fs, "threads").widget, WidgetHint::Number);

    // boolean → two-value Enum (previously plain Text for Snakemake).
    let trim = field(&fs, "trim");
    assert_eq!(trim.widget, WidgetHint::Enum);
    assert_eq!(trim.enum_values.as_deref(), Some(&["true".to_string(), "false".to_string()][..]));

    // The asymmetry fix: a formatted path is detected + HEAD-checked.
    let r = field(&fs, "ref");
    assert_eq!(r.widget, WidgetHint::S3File);
    assert!(r.is_path_input && r.required && !r.is_output);

    assert_eq!(field(&fs, "note").widget, WidgetHint::Text);
}
