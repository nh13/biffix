use serde_json::json;

use super::*;
use crate::formschema::lint::undocumented_params;
use crate::manifest::BiffixManifest;

fn overlaid(m: &BiffixManifest, schema: &Value) -> FormSchema {
    let derived = nf_adapt(schema, m);
    apply_overlay(derived, m.form.as_ref().unwrap(), &no_files)
}

#[test]
fn nf_help_text_is_the_help_and_its_first_sentence_the_description() {
    let fs = nf_adapt(
        &json!({"properties": {
            "aligner": {"type": "string",
                        "help_text": "Which aligner maps the reads. STAR suits most experiments."},
            "trim": {"type": "boolean", "description": "Trim adapters",
                     "help_text": "Trimming removes adapter read-through."},
            "plain": {"type": "string", "description": "  Just this.  "},
            "blank": {"type": "string", "description": "  ", "help_text": ""}
        }}),
        &BiffixManifest::default(),
    );
    let aligner = field(&fs, "aligner");
    assert_eq!(aligner.description.as_deref(), Some("Which aligner maps the reads."));
    assert_eq!(
        aligner.help.as_deref(),
        Some("Which aligner maps the reads. STAR suits most experiments.")
    );
    let trim = field(&fs, "trim");
    assert_eq!(trim.description.as_deref(), Some("Trim adapters"));
    assert_eq!(trim.help.as_deref(), Some("Trimming removes adapter read-through."));
    assert_eq!(field(&fs, "plain").description.as_deref(), Some("Just this."));
    assert_eq!(field(&fs, "plain").help, None);
    // Blank text is no text.
    assert_eq!(field(&fs, "blank").description, None);
    assert_eq!(field(&fs, "blank").help, None);
}

#[test]
fn a_unit_comes_from_the_schema_and_a_hint_wins() {
    let fs = nf_adapt(
        &json!({"properties": {
            "window": {"type": "integer", "unit": "bp"},
            "depth": {"type": "integer", "x-unit": "reads"},
            "mem": {"type": "integer", "unit": "MB"}
        }}),
        &manifest("params:\n  mem:\n    unit: GB\n"),
    );
    assert_eq!(field(&fs, "window").unit.as_deref(), Some("bp"));
    assert_eq!(field(&fs, "depth").unit.as_deref(), Some("reads"));
    assert_eq!(field(&fs, "mem").unit.as_deref(), Some("GB"));
    assert!(fs.warnings.is_empty(), "{:?}", fs.warnings);
}

#[test]
fn biffix_yaml_param_help_is_the_help_for_either_engine() {
    let m = manifest(
        "params:\n  aligner:\n    help: |\n      STAR for most runs.\n      HISAT2 when memory is short.\n    unit: ' '\n",
    );
    let nf = nf_adapt(
        &json!({"properties": {"aligner": {"type": "string", "help_text": "Schema help."}}}),
        &m,
    );
    let help = Some("STAR for most runs.\nHISAT2 when memory is short.");
    assert_eq!(field(&nf, "aligner").help.as_deref(), help);
    assert_eq!(field(&nf, "aligner").description.as_deref(), Some("STAR for most runs."));
    // A blank unit hint is none.
    assert_eq!(field(&nf, "aligner").unit, None);
    let sm = sm_adapt(
        &json!({"properties": {"aligner": {"type": "string", "description": "Aligner"}}}),
        &m,
    );
    assert_eq!(field(&sm, "aligner").description.as_deref(), Some("Aligner"));
    assert_eq!(field(&sm, "aligner").help.as_deref(), help);
    // `help`/`unit` are known keys now, not warned about.
    assert!(m.unknown_keys().is_empty(), "{:?}", m.unknown_keys());
}

#[test]
fn snakemake_descriptions_and_units_reach_the_form() {
    let sm = sm_adapt(
        &json!({"properties": {
            "samples": {"type": "string", "description": "The sample table"},
            "min_mapq": {"type": "integer", "description": "Minimum MAPQ", "unit": "phred"}
        }, "required": ["samples"]}),
        &BiffixManifest::default(),
    );
    assert_eq!(field(&sm, "samples").description.as_deref(), Some("The sample table"));
    assert_eq!(field(&sm, "min_mapq").unit.as_deref(), Some("phred"));
}

#[test]
fn the_form_overlay_help_and_unit_still_win() {
    let m = manifest(
        "params:\n  depth:\n    help: Top-level help.\n    unit: x\nform:\n  params:\n    depth:\n      help: Overlay help.\n      unit: reads\n",
    );
    let schema = json!({"properties": {"depth": {"type": "integer"}}});
    let derived = nf_adapt(&schema, &m);
    assert_eq!(field(&derived, "depth").help.as_deref(), Some("Top-level help."));
    assert_eq!(field(&derived, "depth").description.as_deref(), Some("Top-level help."));
    let form = overlaid(&m, &schema);
    assert_eq!(field(&form, "depth").help.as_deref(), Some("Overlay help."));
    assert_eq!(field(&form, "depth").unit.as_deref(), Some("reads"));
    // The description follows the help that won, not the one it replaced.
    assert_eq!(field(&form, "depth").description.as_deref(), Some("Overlay help."));
}

#[test]
fn the_form_overlay_help_reads_as_the_top_level_hint_does() {
    // Overlay-only help describes a param the schema does not; a schema description is kept;
    // blank overlay text is absent (the schema's unit stays).
    let m = manifest(
        "form:\n  params:\n    x:\n      help: '  Aligner, e.g. STAR. More.  '\n    y:\n      help: Other words.\n    z:\n      unit: '  '\n      help: ''\n",
    );
    let schema = json!({"properties": {
        "x": {"type": "string"},
        "y": {"type": "string", "description": "Schema says."},
        "z": {"type": "integer", "unit": "reads", "help_text": "Depth. Per sample."}
    }});
    let form = overlaid(&m, &schema);
    let x = field(&form, "x");
    assert_eq!(x.help.as_deref(), Some("Aligner, e.g. STAR. More."));
    assert_eq!(x.description.as_deref(), Some("Aligner, e.g. STAR."));
    let y = field(&form, "y");
    assert_eq!(y.help.as_deref(), Some("Other words."));
    assert_eq!(y.description.as_deref(), Some("Schema says."));
    let z = field(&form, "z");
    assert_eq!(z.unit.as_deref(), Some("reads"));
    assert_eq!(z.help.as_deref(), Some("Depth. Per sample."));
    assert_eq!(z.description.as_deref(), Some("Depth."));
}

#[test]
fn an_overlay_title_is_frozen_trimmed() {
    let schema = json!({"properties": {"x": {"type": "string"}}});
    let m = manifest("form:\n  title: |\n    Bulk RNA-seq\n");
    assert_eq!(overlaid(&m, &schema).title.as_deref(), Some("Bulk RNA-seq"));
    let m = manifest("form:\n  title: '  '\n");
    assert_eq!(overlaid(&m, &schema).title, None);
}

#[test]
fn nf_help_text_with_an_abbreviation_or_markdown_describes_by_a_whole_sentence() {
    let fs = nf_adapt(
        &json!({"properties": {
            "fasta": {"type": "string", "help_text": "Path to FASTA, e.g. GRCh38. Used by every step."},
            "steps": {"type": "string", "help_text": "1. Run STAR\n2. Run Salmon"},
            "nb": {"type": "string", "help_text": "**NB:** slow on `big` genomes. Other."},
            "pick": {"type": "string", "help_text": "Choose one of:\n- star\n- hisat2"}
        }}),
        &BiffixManifest::default(),
    );
    assert_eq!(field(&fs, "fasta").description.as_deref(), Some("Path to FASTA, e.g. GRCh38."));
    assert_eq!(field(&fs, "steps").description.as_deref(), Some("Run STAR"));
    assert_eq!(field(&fs, "nb").description.as_deref(), Some("NB: slow on big genomes."));
    assert_eq!(field(&fs, "pick").description, None);
    // The help itself is kept whole.
    assert_eq!(field(&fs, "pick").help.as_deref(), Some("Choose one of:\n- star\n- hisat2"));
}

#[test]
fn a_reference_dropdown_keeps_its_help() {
    let m = manifest(
        "form:\n  references:\n    fill: {fasta: fasta}\n    help: '  Pick the assembly your samples were sequenced against.  '\n",
    );
    let form =
        overlaid(&m, &json!({"properties": {"fasta": {"type": "string", "format": "file-path"}}}));
    assert_eq!(
        form.references.unwrap().help.as_deref(),
        Some("Pick the assembly your samples were sequenced against.")
    );
}

#[test]
fn the_lint_names_shown_params_that_explain_nothing() {
    let fs = nf_adapt(
        &json!({"$defs": {
            "main": {"required": ["input"], "properties": {
                "input": {"type": "string"},
                "described": {"type": "string", "description": "Said."},
                "helped": {"type": "string", "help_text": "Said."},
                "quiet": {"type": "string"},
                "tucked": {"type": "string", "hidden": true}
            }},
            "advanced": {"properties": {
                "deep": {"type": "string"}
            }}
        }}),
        &BiffixManifest::default(),
    );
    let missing: Vec<&str> = undocumented_params(&fs).iter().map(|f| f.name.as_str()).collect();
    // `deep` is in an advanced section (it has no required param); `tucked` is hidden.
    assert_eq!(missing, ["input", "quiet"]);
    assert_eq!(
        help_lint(&fs).unwrap(),
        "2 parameter(s) the launch form shows have no description or help: input, quiet. A \
             biologist sees only the name; add a `description` (or `help_text`) in the schema, \
             or `help:` under `params.<name>` in biffix.yaml"
    );
}

#[test]
fn the_lint_skips_reference_filled_params_counts_the_rest_and_is_quiet_when_all_are_said() {
    let props: serde_json::Map<String, Value> = (0..10)
        .map(|i| (format!("p{i}"), json!({"type": "string"})))
        .chain([("fasta".to_string(), json!({"type": "string"}))])
        .collect();
    let required: Vec<String> = props.keys().cloned().collect();
    let m = manifest("form:\n  references:\n    fill: {fasta: fasta}\n");
    let form = overlaid(&m, &json!({"properties": props, "required": required}));
    let lint = help_lint(&form).unwrap();
    assert!(lint.starts_with("10 parameter(s)"), "{lint}");
    assert!(lint.contains(", and 2 more."), "{lint}");
    assert!(!lint.contains("fasta"), "{lint}");

    let said = nf_adapt(
        &json!({"properties": {"x": {"type": "string", "description": "X"}}, "required": ["x"]}),
        &BiffixManifest::default(),
    );
    assert_eq!(help_lint(&said), None);
}
