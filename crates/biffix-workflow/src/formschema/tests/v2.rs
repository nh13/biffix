use serde_json::json;

use super::*;
use crate::manifest::BiffixManifest;

/// An nf-core-shaped schema: groups listed out of alphabetical order, with titles, hidden
/// params, defaults, ranges and patterns.
const NF_SCHEMA: &str = r##"{
        "$defs": {
            "reference_genome_options": {
                "title": "Reference genome options",
                "properties": {
                    "genome": { "type": "string", "description": "iGenomes id" },
                    "fasta": { "type": "string", "format": "file-path" }
                }
            },
            "input_output_options": {
                "title": "Input/output options",
                "description": "Where the data lives.",
                "required": ["input"],
                "properties": {
                    "input": { "type": "string", "format": "file-path", "schema": "assets/schema_input.json",
                               "pattern": "^\\S+\\.csv$" },
                    "outdir": { "type": "string", "format": "directory-path" },
                    "email": { "type": "string", "hidden": true }
                }
            },
            "trimming_options": {
                "properties": {
                    "min_trimmed_reads": { "type": "integer", "default": 10000, "minimum": 0,
                                           "help_text": "Samples with fewer reads are dropped." },
                    "trimmer": { "type": "string", "enum": ["trimgalore", "fastp"], "default": "trimgalore" }
                }
            }
        },
        "allOf": [
            { "$ref": "#/$defs/input_output_options" },
            { "$ref": "#/$defs/reference_genome_options" },
            { "$ref": "#/$defs/trimming_options" }
        ]
    }"##;

fn nf_form(manifest: &BiffixManifest) -> FormSchema {
    let (value, order) = parse_schema_text(NF_SCHEMA).unwrap();
    from_nextflow_schema_ordered(&value, &order, manifest, &no_files)
}

fn names(form: &FormSchema) -> Vec<&str> {
    form.fields.iter().map(|f| f.name.as_str()).collect()
}

#[test]
fn nf_schema_groups_become_sections_in_allof_and_file_order() {
    let form = nf_form(&BiffixManifest::default());
    let sections: Vec<(&str, &str, bool)> =
        form.sections.iter().map(|s| (s.id.as_str(), s.title.as_str(), s.advanced)).collect();
    assert_eq!(
        sections,
        vec![
            ("input_output_options", "Input/output options", false),
            ("reference_genome_options", "Reference genome options", true),
            ("trimming_options", "Trimming options", true),
        ]
    );
    assert_eq!(
        names(&form),
        vec!["input", "outdir", "email", "genome", "fasta", "min_trimmed_reads", "trimmer"]
    );
    assert_eq!(form.sections[0].description.as_deref(), Some("Where the data lives."));
}

#[test]
fn nf_schema_keeps_defaults_help_hidden_ranges_and_patterns() {
    let form = nf_form(&BiffixManifest::default());
    let reads = field(&form, "min_trimmed_reads");
    assert_eq!(reads.default, Some(json!(10000)));
    assert_eq!(reads.min, Some(0.0));
    assert_eq!(reads.step, Some(1.0));
    assert_eq!(reads.help.as_deref(), Some("Samples with fewer reads are dropped."));
    assert!(field(&form, "email").hidden);
    assert_eq!(field(&form, "trimmer").default, Some(json!("trimgalore")));
    assert_eq!(field(&form, "input").rules[0].pattern, r"^\S+\.csv$");
    assert_eq!(field(&form, "genome").section.as_deref(), Some("reference_genome_options"));
}

#[test]
fn snakemake_config_splits_required_inputs_from_other_options_in_file_order() {
    let text = "properties:\n  zeta: {type: string}\n  reads: {type: string}\n  alpha: {type: integer}\nrequired: [reads]\n";
    let (value, order) = parse_schema_text(text).unwrap();
    let form = from_snakemake_config_ordered(&value, &order, &BiffixManifest::default());
    assert_eq!(names(&form), vec!["reads", "zeta", "alpha"]);
    let sections: Vec<(&str, bool)> =
        form.sections.iter().map(|s| (s.title.as_str(), s.advanced)).collect();
    assert_eq!(sections, vec![("Inputs", false), ("Other options", true)]);
}

const MANIFEST: &str = r#"
form:
  title: "Bulk RNA-seq"
  run_name: "rnaseq · {date}"
  sections:
    - { id: samples, title: "Samples" }
    - { id: options, title: "Options", description: "Most runs only need these." }
  params:
    input: { label: "Samples", section: samples }
    trimmer:
      section: options
      presets: { "Trim Galore": trimgalore, "fastp": fastp, "Cutadapt": cutadapt }
    min_trimmed_reads:
      label: "Minimum trimmed reads"
      section: options
      unit: reads
      presets: { "Default (10,000)": 10000, "Strict (100,000)": 100000 }
    fasta: { show_if: { genome: [custom, "" ] } }
    genome: { show_if: { fasta: x } }
    outdir: { hidden: true }
    nope: { label: "Not a param" }
    email: { section: nowhere, show_if: { ghost: true } }
  examples:
    - name: "Smoke"
      params: { trimmer: fastp, bogus: 1 }
      params_file: tests/smoke.yaml
      samplesheet: s3://bucket/smoke.csv
    - name: "Local sheet"
      samplesheet: tests/samples.csv
"#;

/// The `form.references` warnings (the fixture's row-schema warning aside).
fn reference_warnings(form: &FormSchema) -> Vec<&str> {
    form.warnings.iter().map(String::as_str).filter(|w| w.starts_with("form.references")).collect()
}

fn with_references(yaml: &str) -> FormSchema {
    let manifest = BiffixManifest::parse_yaml(&format!("form:\n  references:\n{yaml}")).unwrap();
    apply_overlay(nf_form(&manifest), manifest.form.as_ref().unwrap(), &|_| Err("x".into()))
}

#[test]
fn references_fill_params_and_default_their_required_keys() {
    let form = with_references(
        "    kind: assembly\n    fill: { fasta: fasta, genome: igenome, nope: gtf }\n    \
             section: reference\n    label: Genome\n",
    );
    let refs = form.references.as_ref().unwrap();
    assert_eq!(refs.kind.as_deref(), Some("assembly"));
    assert_eq!(refs.fill.len(), 2);
    assert_eq!(refs.required, ["fasta", "igenome"]);
    assert_eq!(refs.label, "Genome");
    assert!(refs.custom);
    // Filled params move into the dropdown's section, which is added (titled by the label).
    assert_eq!(field(&form, "fasta").section.as_deref(), Some("reference"));
    let section = form.sections.iter().find(|s| s.id == "reference").unwrap();
    assert_eq!(section.title, "Genome");
    assert_eq!(
        reference_warnings(&form),
        ["form.references.fill.nope: not a parameter of this workflow; ignored"]
    );
}

#[test]
fn references_required_keys_and_custom_are_the_authors() {
    let form = with_references(
        "    fill: { fasta: fasta }\n    required: [fasta, gtf]\n    custom: false\n",
    );
    let refs = form.references.as_ref().unwrap();
    assert_eq!(refs.required, ["fasta", "gtf"]);
    assert!(!refs.custom);
    assert_eq!(refs.label, "Reference genome");
    assert_eq!(
        reference_warnings(&form),
        ["form.references.required: `gtf` fills no parameter; it only narrows the bundles offered"]
    );
}

#[test]
fn references_that_fill_nothing_are_dropped() {
    let form = with_references("    fill: { nope: fasta }\n");
    assert!(form.references.is_none());
    assert_eq!(
        reference_warnings(&form),
        [
            "form.references.fill.nope: not a parameter of this workflow; ignored",
            "form.references: fills no parameter; ignored",
        ]
    );
}

fn curated() -> FormSchema {
    let manifest = BiffixManifest::parse_yaml(MANIFEST).unwrap();
    let read = |rel: &str| match rel {
        "tests/smoke.yaml" => Ok("trimmer: trimgalore\nmin_trimmed_reads: 5\n".to_string()),
        other => Err(format!("no {other}")),
    };
    apply_overlay(nf_form(&manifest), manifest.form.as_ref().unwrap(), &read)
}

#[test]
fn overlay_orders_sections_and_places_fields() {
    let form = curated();
    assert!(form.curated);
    assert_eq!(form.title.as_deref(), Some("Bulk RNA-seq"));
    assert_eq!(form.run_name.as_deref(), Some("rnaseq · {date}"));
    let ids: Vec<&str> = form.sections.iter().map(|s| s.id.as_str()).collect();
    // Overlay sections first, then the derived ones still holding fields, then the section the
    // overlay introduced by name only.
    assert_eq!(
        ids,
        vec!["samples", "options", "input_output_options", "reference_genome_options", "nowhere"]
    );
    assert_eq!(
        names(&form),
        vec!["input", "min_trimmed_reads", "trimmer", "outdir", "genome", "fasta", "email"]
    );
    let reads = field(&form, "min_trimmed_reads");
    assert_eq!(reads.label.as_deref(), Some("Minimum trimmed reads"));
    assert_eq!(reads.unit.as_deref(), Some("reads"));
    // Derived facts the overlay did not touch survive.
    assert_eq!(reads.default, Some(json!(10000)));
    assert!(field(&form, "outdir").hidden);
}

#[test]
fn overlay_presets_keep_order_and_respect_choices() {
    let form = curated();
    let labels: Vec<&str> =
        field(&form, "trimmer").presets.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, vec!["Trim Galore", "fastp"]);
    // The valid `min_trimmed_reads` presets earn no warning; only `Cutadapt` does.
    let preset_warnings: Vec<&String> =
        form.warnings.iter().filter(|w| w.contains("preset")).collect();
    assert_eq!(
        preset_warnings,
        ["form.params.trimmer: preset `Cutadapt` is not one of the choices; ignored"]
    );
    assert_eq!(field(&form, "min_trimmed_reads").presets[1].value, json!(100000));
}

#[test]
fn overlay_problems_are_warnings() {
    let form = curated();
    // Every warning the curated fixture earns, and nothing else (a spurious one fails here).
    assert_eq!(
        form.warnings,
        [
            "`input`: cannot read samplesheet schema assets/schema_input.json: no assets/schema_input.json",
            "form.params.email: section `nowhere` is not declared; added",
            "form.params.nope: not a parameter of this workflow; ignored",
            "form.params.trimmer: preset `Cutadapt` is not one of the choices; ignored",
            "form.params.email: show_if names `ghost`, which is not another parameter; ignored",
            "form.params.genome: show_if conditions form a cycle; the field is always shown",
            "form.params.fasta: show_if conditions form a cycle; the field is always shown",
            "example `Smoke`: `bogus` is not a parameter of this workflow; ignored",
            "example `Local sheet`: samplesheet must be an s3:// URI; ignored",
        ]
    );
    assert!(field(&form, "email").show_if.is_empty());
    assert_eq!(field(&form, "fasta").show_if[0].values, vec![json!("custom"), json!("")]);
}

#[test]
fn overlay_examples_merge_params_file_under_inline_params() {
    let form = curated();
    let smoke = &form.examples[0];
    assert_eq!(smoke.params.get("trimmer"), Some(&json!("fastp")));
    assert_eq!(smoke.params.get("min_trimmed_reads"), Some(&json!(5)));
    assert!(!smoke.params.contains_key("bogus"));
    assert_eq!(smoke.samplesheet_uri.as_deref(), Some("s3://bucket/smoke.csv"));
    assert_eq!(form.examples[1].samplesheet_uri, None);
    let example_warnings: Vec<&String> =
        form.warnings.iter().filter(|w| w.starts_with("example ")).collect();
    assert_eq!(
        example_warnings,
        [
            "example `Smoke`: `bogus` is not a parameter of this workflow; ignored",
            "example `Local sheet`: samplesheet must be an s3:// URI; ignored",
        ]
    );
}

#[test]
fn form_params_hints_choose_the_widget() {
    let manifest =
        BiffixManifest::parse_yaml("form:\n  params:\n    genome: { format: reference-bundle }\n")
            .unwrap();
    let form = nf_form(&manifest);
    assert_eq!(field(&form, "genome").widget, WidgetHint::ReferenceBundle);
}

#[test]
fn a_form_frozen_before_v2_still_deserializes() {
    let old = json!({
        "fields": [{ "name": "reads", "widget": "s3_file", "required": true,
                     "is_output": false, "is_secret": false, "is_path_input": true }],
        "warnings": []
    });
    let form: FormSchema = serde_json::from_value(old).unwrap();
    assert!(form.sections.is_empty() && !form.curated);
    assert_eq!(form.fields[0].section, None);
}

#[test]
fn value_key_equates_spellings() {
    assert_eq!(value_key(&json!(true)), value_key(&json!("true")));
    assert_eq!(value_key(&json!(3)), value_key(&json!(3.0)));
    assert_eq!(value_key(&json!(3)), "3");
    assert_eq!(value_key(&json!(2.5)), "2.5");
    assert_eq!(value_key(&Value::Null), "");
    // Whole as long as an f64 holds it exactly, as the UI's `String(n)` prints it.
    assert_eq!(value_key(&json!(1e15)), "1000000000000000");
    assert_eq!(value_key(&json!(1e15)), value_key(&json!(1_000_000_000_000_000_u64)));
}

#[test]
fn blank_overlay_text_is_frozen_as_none() {
    let manifest = BiffixManifest::parse_yaml(
        "form:\n  description: '  '\n  run_name: ''\n  params:\n    n:\n      label: ' '\n      \
         placeholder: '  '\n",
    )
    .unwrap();
    let form =
        sm_adapt(&json!({"properties": {"n": {"type": "string", "title": "Count"}}}), &manifest);
    let form = apply_overlay(form, manifest.form.as_ref().unwrap(), &|_| Err("x".into()));
    assert_eq!(form.description, None);
    assert_eq!(form.run_name, None);
    assert_eq!(field(&form, "n").placeholder, None);
    assert_ne!(field(&form, "n").label.as_deref(), Some(" "));
}
