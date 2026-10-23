//! The adapters' shared field derivation: one schema property plus its `biffix.yaml` hint becomes a
//! [`FormField`] (widget, choices, bounds, help), and params with no grouping of their own fall
//! into the fallback sections.

use serde_json::Value;

use crate::about::help_description;
use crate::formschema::{value_key, FormField, FormSchema, FormSection, Rule, WidgetHint};
use crate::manifest::{BiffixManifest, ParamHint};
use crate::validate::NEXTFLOW_OUTDIR_PARAM;

/// Turn `snake_case_name` into `Snake case name` — the title of a section with none of its own.
pub fn humanize(name: &str) -> String {
    let spaced = name.replace(['_', '-'], " ");
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A param normalized out of an engine's schema dialect into the shape the shared widget picker
/// reasons over. Both adapters normalize into this, so the manifest-hint ladder and the widget
/// rules live in exactly one place.
struct ParamShape<'a> {
    name: &'a str,
    /// JSON Schema `type` (integer/number/boolean/string/…); of a nullable type
    /// (`["integer", "null"]`), the first that is not `null`.
    json_type: Option<&'a str>,
    /// JSON Schema `format`. nf-schema uses file-path/directory-path/path; a Snakemake config may
    /// also carry `format`, in which case it gets the same path detection (fixes the old asymmetry
    /// where an un-hinted Snakemake path fell back to plain Text and was never HEAD-checked).
    format: Option<&'a str>,
    has_enum: bool,
    /// The nf-core samplesheet convention: a param carrying its own row `schema` sub-key.
    has_row_schema: bool,
    /// Adapting an nf-schema: Nextflow conventions (the `outdir` output folder) apply.
    nextflow: bool,
}

/// The `format` values a `biffix.yaml` param hint may name.
const HINT_FORMATS: &str = "s3-file, s3-dir, reference-bundle, samplesheet";

/// What the widget picker decided for a param.
#[derive(Default)]
struct Picked {
    widget: WidgetHint,
    is_output: bool,
    is_secret: bool,
    is_path_input: bool,
    accepts_folder: bool,
}

impl Picked {
    fn plain(widget: WidgetHint) -> Self {
        Picked { widget, ..Picked::default() }
    }

    /// A path: an output (never existence-checked) or an input (checked).
    fn path(widget: WidgetHint, output: bool) -> Self {
        Picked { widget, is_output: output, is_path_input: !output, ..Picked::default() }
    }
}

/// The ONE widget picker. Manifest hint first (the shared secret→s3→bundle→samplesheet
/// ladder), then the param's shape. A hint it cannot honor (an unknown `format`, `output` on a
/// param that is not a path) is a warning, never silently dropped.
fn pick_widget(hint: Option<&ParamHint>, shape: &ParamShape, warnings: &mut Vec<String>) -> Picked {
    let name = shape.name;
    let output = hint.is_some_and(|h| h.output);
    let output_ignored = |warnings: &mut Vec<String>| {
        if output {
            warnings.push(format!(
                "params.{name}: `output: true` applies only to a file or folder path; ignored"
            ));
        }
    };
    if let Some(h) = hint {
        // `format: secret` is how an author guesses `secret: true`; honor it, since a value meant
        // to be secret must never be echoed, and name the right spelling.
        let format_secret = h.format.as_deref() == Some("secret");
        if format_secret {
            warnings.push(format!(
                "params.{name}: `format: secret` is spelled `secret: true`; treated as a secret"
            ));
        }
        if h.secret || format_secret {
            output_ignored(warnings);
            return Picked { is_secret: true, ..Picked::plain(WidgetHint::Text) };
        }
        match h.format.as_deref() {
            Some("s3-file") => return Picked::path(WidgetHint::S3File, output),
            Some("s3-dir") => return Picked::path(WidgetHint::S3Dir, output),
            Some("reference-bundle") => {
                output_ignored(warnings);
                return Picked::plain(WidgetHint::ReferenceBundle);
            }
            // Not existence-checked: a hinted samplesheet may name a file inside the workflow repo.
            Some("samplesheet") => {
                output_ignored(warnings);
                return Picked::plain(WidgetHint::Samplesheet);
            }
            Some(other) => warnings.push(format!(
                "params.{name}: format `{other}` is not one of {HINT_FORMATS}; ignored"
            )),
            None => {}
        }
    }
    // nf-core samplesheet: an input param that carries its own row schema.
    if shape.has_row_schema {
        output_ignored(warnings);
        return Picked::path(WidgetHint::Samplesheet, false);
    }
    // JSON Schema `format` path types — honored for either engine when present.
    match shape.format {
        Some("file-path") => return Picked::path(WidgetHint::S3File, output),
        // nf-schema `path` is a file or a directory: offered as a file, checked as either.
        Some("path") => {
            return Picked { accepts_folder: !output, ..Picked::path(WidgetHint::S3File, output) }
        }
        Some("directory-path") => {
            // Nextflow's `outdir` is where results are written (nf-core); every other directory
            // param is an input unless the author marks it `output: true`.
            let conventional = shape.nextflow && name == NEXTFLOW_OUTDIR_PARAM;
            if !shape.nextflow && name == NEXTFLOW_OUTDIR_PARAM && !output {
                warnings.push(format!(
                    "param `{name}` is checked as an input folder; add `output: true` under \
                     `params.{name}` in biffix.yaml if the workflow writes to it"
                ));
            }
            return Picked::path(WidgetHint::S3Dir, output || conventional);
        }
        _ => {}
    }
    output_ignored(warnings);
    if shape.has_enum {
        return Picked::plain(WidgetHint::Enum);
    }
    match shape.json_type {
        Some("integer" | "number") => Picked::plain(WidgetHint::Number),
        // No boolean widget — render a two-value choice instead.
        Some("boolean") => Picked::plain(WidgetHint::Enum),
        _ => Picked::plain(WidgetHint::Text),
    }
}

/// A schema `type`: the string, or of a nullable list (`["integer", "null"]`) the first non-null
/// entry.
fn schema_type(obj: &serde_json::Map<String, Value>) -> Option<&str> {
    match obj.get("type")? {
        Value::String(t) => Some(t),
        Value::Array(types) => types.iter().filter_map(Value::as_str).find(|t| *t != "null"),
        _ => None,
    }
}

/// A schema `enum`'s choices, each scalar member spelled by [`value_key`] (so `[1, 2]` offers
/// `"1"`, `"2"` and `[true, false]` offers `"true"`, `"false"`). A `null` member (a nullable
/// enum) is dropped, since not giving the param is already how a form says "none"; an object or
/// array member cannot be a choice and is dropped with a warning. No choices left ⇒ no enum.
fn enum_choices(
    name: &str,
    value: Option<&Value>,
    warnings: &mut Vec<String>,
) -> Option<Vec<String>> {
    let members = value?.as_array()?;
    let mut choices: Vec<String> = Vec::new();
    for member in members {
        match member {
            Value::Null => {}
            Value::Array(_) | Value::Object(_) => warnings.push(format!(
                "param `{name}`: enum member {member} is not a plain value; dropped"
            )),
            scalar => {
                let key = value_key(scalar);
                if !choices.contains(&key) {
                    choices.push(key);
                }
            }
        }
    }
    if choices.is_empty() {
        warnings.push(format!("param `{name}`: enum lists no choices; any value is accepted"));
        return None;
    }
    Some(choices)
}

/// Whether the server can check `pattern` (the Rust `regex` dialect: no look-around, no
/// backreferences). One it cannot is a warning; the caller drops the rule, so the frozen form
/// states only what is enforced (the server would skip it at submit anyway).
pub(super) fn checkable_pattern(context: &str, pattern: &str, warnings: &mut Vec<String>) -> bool {
    match regex::Regex::new(pattern) {
        Ok(_) => true,
        Err(e) => {
            let reason = crate::text::regex_error_reason(&e);
            warnings.push(format!(
                "{context}: pattern `{pattern}` cannot be checked by the server ({reason}); dropped"
            ));
            false
        }
    }
}

/// Overlay text trimmed, blank as absent.
pub(super) fn nonblank(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|t| !t.is_empty()).map(String::from)
}

/// Build one FormField from a param's JSON Schema object + manifest hint — shared by both engine
/// adapters, so widget selection, the enum extraction, and the boolean→two-value synthesis
/// happen once. `help` is the hint's (`params.<name>.help`), else the schema's `help_text`; with no
/// `description`, the help's first sentence describes the param ([`help_description`]). `unit` is
/// the hint's, else the schema's `unit` (or `x-unit`). Problems go to `warnings`.
pub(super) fn field_from(
    name: &str,
    obj: &serde_json::Map<String, Value>,
    hint: Option<&ParamHint>,
    required: bool,
    section: &str,
    nextflow: bool,
    warnings: &mut Vec<String>,
) -> FormField {
    let json_type = schema_type(obj);
    let mut enum_values = enum_choices(name, obj.get("enum"), warnings);
    let shape = ParamShape {
        name,
        json_type,
        format: obj.get("format").and_then(|f| f.as_str()),
        has_enum: enum_values.is_some(),
        has_row_schema: obj.get("schema").and_then(|s| s.as_str()).is_some(),
        nextflow,
    };
    let picked = pick_widget(hint, &shape, warnings);
    // Synthesize the choices for a boolean rendered as a two-value Enum.
    if enum_values.is_none() && json_type == Some("boolean") && picked.widget == WidgetHint::Enum {
        enum_values = Some(vec!["true".into(), "false".into()]);
    }
    // Only a path input takes a list (the validator reads `multiple` for path inputs only).
    let is_path = matches!(picked.widget, WidgetHint::S3File | WidgetHint::S3Dir);
    if hint.is_some_and(|h| h.multiple) && !is_path {
        warnings.push(format!(
            "params.{name}: `multiple: true` applies only to a file or folder path; ignored"
        ));
    }
    // Blank text says nothing, so it counts as absent.
    let text = |key: &str| nonblank(obj.get(key).and_then(|d| d.as_str()));
    let help = nonblank(hint.and_then(|h| h.help.as_deref())).or_else(|| text("help_text"));
    // A param with only a help paragraph is described inline by its first sentence; the whole
    // paragraph stays the help.
    let description = text("description").or_else(|| help.as_deref().and_then(help_description));
    let unit = nonblank(hint.and_then(|h| h.unit.as_deref()))
        .or_else(|| text("unit"))
        .or_else(|| text("x-unit"));
    let rules = text("pattern")
        .filter(|pattern| checkable_pattern(&format!("param `{name}`"), pattern, warnings))
        .map(|pattern| Rule { message: format!("must match {pattern}"), pattern })
        .into_iter()
        .collect();
    let number = |key: &str| obj.get(key).and_then(Value::as_f64);
    let (mut min, mut max) = (number("minimum"), number("maximum"));
    // Draft 2019+ spells an exclusive bound as a number; draft 4 as `true` beside `minimum`.
    let mut exclusive_min = number("exclusiveMinimum");
    if obj.get("exclusiveMinimum").and_then(Value::as_bool) == Some(true) {
        exclusive_min = min.take();
    }
    let mut exclusive_max = number("exclusiveMaximum");
    if obj.get("exclusiveMaximum").and_then(Value::as_bool) == Some(true) {
        exclusive_max = max.take();
    }
    let multiple_of = number("multipleOf");
    if multiple_of.is_some_and(|m| m <= 0.0) {
        warnings.push(format!("param `{name}`: multipleOf must be greater than 0; ignored"));
    }
    FormField {
        name: name.to_string(),
        widget: picked.widget,
        required,
        description,
        enum_values,
        is_output: picked.is_output,
        is_secret: picked.is_secret,
        is_path_input: picked.is_path_input,
        multiple: is_path && hint.is_some_and(|h| h.multiple),
        json_type: json_type.map(String::from),
        accepts_folder: picked.accepts_folder,
        help,
        unit,
        section: Some(section.to_string()),
        hidden: obj.get("hidden").and_then(serde_json::Value::as_bool).unwrap_or(false),
        default: obj.get("default").filter(|d| !d.is_null()).cloned(),
        min,
        max,
        exclusive_min,
        exclusive_max,
        multiple_of: multiple_of.filter(|m| *m > 0.0),
        step: (json_type == Some("integer")).then_some(1.0),
        rules,
        suffixes: hint
            .and_then(|h| h.suffix.as_deref())
            .map(|s| {
                s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
            })
            .unwrap_or_default(),
        ..FormField::default()
    }
}

/// The section a Snakemake (or an ungrouped nf-schema) param lands in when the schema has no
/// grouping of its own: required params are shown, optional ones collapse under *Advanced*.
pub(super) const REQUIRED_SECTION: &str = "inputs";
pub(super) const OTHER_SECTION: &str = "other";

pub(super) fn fallback_sections(fields: &[FormField]) -> Vec<FormSection> {
    let mut sections = Vec::new();
    for (id, title, advanced) in
        [(REQUIRED_SECTION, "Inputs", false), (OTHER_SECTION, "Other options", true)]
    {
        if fields.iter().any(|f| f.section.as_deref() == Some(id)) {
            sections.push(FormSection {
                id: id.into(),
                title: title.into(),
                description: None,
                advanced,
            });
        }
    }
    sections
}
/// A top-level `params:` hint for a param the schema does not have does nothing; say so. (A
/// `form.params` entry is folded into `params` too, but [`apply_overlay`](super::apply_overlay)
/// already warns about it.)
pub(super) fn warn_unmatched_hints(form: &mut FormSchema, manifest: &BiffixManifest) {
    let in_overlay =
        |name: &String| manifest.form.as_ref().is_some_and(|f| f.params.contains_key(name));
    for name in manifest.params.keys() {
        if !in_overlay(name) && !form.fields.iter().any(|f| &f.name == name) {
            form.warnings.push(format!("params.{name}: not a parameter of this workflow; ignored"));
        }
    }
}
