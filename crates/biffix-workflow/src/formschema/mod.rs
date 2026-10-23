//! The normalized internal **FormSchema**.
//!
//! The UI renders FormSchema only; adapters translate each engine's schema dialect into it. The
//! Snakemake adapter reads a JSON-Schema-shaped config, leaning on `biffix.yaml` param hints for
//! the semantics the config schema lacks. Adapters are **lenient** — a malformed field is
//! dropped with a warning, never a hard abort.
//!
//! Lives in biffix-workflow so the CLI derives the FormSchema at register time from the same
//! adapter the backend uses.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod catalog;
mod fields;
mod lint;
mod nextflow;
mod overlay;
mod reports;
mod schema_text;
mod snakemake;
#[cfg(test)]
mod tests;

pub use catalog::{
    attach_catalog, normalize_catalog, normalize_contact, Contact, FormCatalog,
    MAX_CATALOG_DESCRIPTION_CHARS, MAX_CATALOG_TAGS, MAX_CATALOG_TAG_CHARS, MAX_CONTACT_NAME_CHARS,
    MAX_CONTACT_URL_CHARS,
};
pub use fields::humanize;
pub use lint::help_lint;
pub use nextflow::from_nextflow_schema_ordered;
pub use overlay::apply_overlay;
pub use reports::{
    attach_reports, normalize_reports, report_format, FormReport, REPORT_KINDS, SAMPLE_PLACEHOLDER,
};
pub(crate) use schema_text::mapping_keys;
pub use schema_text::{parse_schema_text, SchemaOrder};
pub use snakemake::from_snakemake_config_ordered;

/// Collect the string elements of a JSON array value (a schema's `required` list of names),
/// returning `None` when the value is absent or not an array; non-string elements are skipped. Both
/// adapters use it so the `filter_map`-over-`as_array` idiom lives in one place. Not for `enum`,
/// whose members may be any scalar (see `fields::enum_choices`).
fn json_string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let array = value?.as_array()?;
    Some(array.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

/// The semantic widget hint. Keeps the UI decoupled from schema dialects.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetHint {
    #[default]
    Text,
    Number,
    Enum,
    S3File,
    S3Dir,
    ReferenceBundle,
    Samplesheet,
}

/// Stable wire/UI placeholder that replaces a secret field's value in every recorded copy of a
/// run's params (the control plane redacts before the `run_submitted` anchor is built).
pub const REDACTED_SECRET_PLACEHOLDER: &str = "[REDACTED]";

fn is_false(b: &bool) -> bool {
    !*b
}

/// A named value for a field, shown as a chip (`Duplex (3)` → `3`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Preset {
    pub label: String,
    pub value: Value,
}

/// One condition of a field's `show_if`: the field is shown only while `field`'s value is one of
/// `values` (compared by [`value_key`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowIf {
    pub field: String,
    pub values: Vec<Value>,
}

/// A pattern a string value must match, with the message shown when it does not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rule {
    pub pattern: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormField {
    pub name: String,
    pub widget: WidgetHint,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    /// Marks an output path — the validator skips existence-checking it.
    pub is_output: bool,
    /// Stored encrypted; never echoed/logged.
    pub is_secret: bool,
    /// True for path-typed inputs that should be HEAD-checked.
    pub is_path_input: bool,
    /// The param takes a list of paths (`multiple: true` in biffix.yaml)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
    /// The param's JSON Schema `type` (`boolean`, `integer`, `string`…), when the schema declares
    /// one; absent on forms frozen before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_type: Option<String>,
    /// A path input that may name a file **or** a folder (nf-schema `format: path`): the server
    /// checks it exists as an object, else as a prefix. Rendered as an `s3_file` field.
    #[serde(default, skip_serializing_if = "is_false")]
    pub accepts_folder: bool,
    /// The author's label; absent ⇒ the UI humanizes `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A help paragraph (nf-schema `help_text`, or the author's), longer than `description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// The id of the [`FormSection`] the field belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Shown only under *Show all parameters*.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// The value the engine uses when the param is not given (the engine schema's `default`). It is
    /// what `show_if` gating assumes for an omitted param on every surface (server, CLI, UI), so it
    /// is never the author's form prefill: that is [`FormField::prefill`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// The value a fresh launch form starts with (the `form.params.<name>.default` overlay). The UI
    /// fills it in as an entered value, so it is submitted explicitly; a CLI/API submit that omits
    /// the param gets [`FormField::default`], exactly as the engine would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// A unit shown with the label (`bp`, `reads`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiline: bool,
    /// Inclusive lower bound (JSON Schema `minimum`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Inclusive upper bound (JSON Schema `maximum`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Exclusive lower bound (JSON Schema `exclusiveMinimum`): a value must be greater.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_min: Option<f64>,
    /// Exclusive upper bound (JSON Schema `exclusiveMaximum`): a value must be less.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_max: Option<f64>,
    /// A value must be a whole multiple of this (JSON Schema `multipleOf`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_of: Option<f64>,
    /// The number input's increment, a UI hint only: `1` for an integer param unless the overlay
    /// sets another. Whole numbers are enforced by `json_type` (`integer`), not by this; a form
    /// frozen before `json_type` was recorded falls back to `step: 1` as the integer marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
    /// All must hold for the field to be active; an inactive field is dropped before validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub show_if: Vec<ShowIf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    /// File-name endings a path input accepts (`.fastq.gz`), from the `suffix` hint; the file
    /// picker filters by them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suffixes: Vec<String>,
    /// A samplesheet field's columns (from nf-core's row schema or the author's `form:` block), so
    /// the form can build the sheet from files and the server can write it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samplesheet: Option<crate::samples::SheetSpec>,
}

/// A titled group of fields. Non-advanced sections are cards; advanced ones collapse together under
/// one *Advanced* disclosure.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormSection {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub advanced: bool,
}

/// A named, known-good set of inputs the author offers (*Start from example*).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormExample {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Param values keyed by field name.
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samplesheet_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormSchema {
    /// Fields in display order: grouped by section, in the source schema's order within one.
    pub fields: Vec<FormField>,
    /// Non-fatal issues encountered while adapting (lenient skip-and-warn).
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Sections in display order. Empty for a form frozen before sections existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<FormSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<FormExample>,
    /// A run-name template; `{date}` and `{workflow}` are filled by the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    /// True when the author curated this form (`biffix.yaml` has a `form:` block).
    #[serde(default, skip_serializing_if = "is_false")]
    pub curated: bool,
    /// A reference-bundle dropdown that fills several params, when the author declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<FormReferences>,
    /// Reports the workflow's runs write, shown on a run's Reports tab (`reports:` in
    /// `biffix.yaml`). Frozen with the form so every run of the version resolves the same
    /// declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<FormReport>,
    /// How to match the workflow's tasks to samples (`samples:` in `biffix.yaml`), frozen with the
    /// form so every run of the version attributes its tasks the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<biffix_core::event::SampleHint>,
    /// How the version is listed in the Lab's pipeline catalog (`catalog:` in `biffix.yaml`),
    /// normalized at registration (see [`normalize_catalog`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<FormCatalog>,
    /// Snakemake: reuse the results of the workflow's `cache: True` rules across runs, from a
    /// per-project S3 cache (`cache: true` in `biffix.yaml`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub cache: bool,
}
/// Attach the manifest's `samples:` hint to `form`. A tag pattern that is not a valid regex, or has
/// no `sample` group, is dropped with a warning (it could never name a sample); a step scope this
/// build does not know is warned about and kept (it forces nothing).
pub fn attach_samples(form: &mut FormSchema, hint: Option<&biffix_core::event::SampleHint>) {
    let Some(hint) = hint else { return };
    let mut hint = hint.clone();
    for (step, scope) in &hint.steps {
        if let biffix_core::event::StepScope::Unknown(scope) = scope {
            form.warnings.push(format!(
                "samples.steps.{step}: unknown scope `{scope}` (sample or study); ignored"
            ));
        }
    }
    if let Some(tag) = &hint.tag {
        let usable = regex::Regex::new(tag)
            .map(|re| re.capture_names().flatten().any(|n| n == "sample"))
            .unwrap_or(false);
        if !usable {
            form.warnings.push(format!(
                "samples.tag: `{tag}` must be a regex with a `(?P<sample>…)` group; ignored"
            ));
            hint.tag = None;
        }
    }
    if !hint.is_empty() {
        form.samples = Some(hint);
    }
}
/// A reference-bundle dropdown: picking a bundle fills each `fill` param from
/// the bundle's key of the same mapping; only bundles with every `required` key are offered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormReferences {
    /// Offer only bundles of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Param name → bundle key.
    pub fill: std::collections::BTreeMap<String, String>,
    /// Bundle keys a bundle must have to be offered.
    pub required: Vec<String>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Offer *Use my own files…*: the filled params' own fields instead of a bundle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub custom: bool,
    /// A help paragraph for the dropdown's ⓘ (which reference suits which samples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// The comparable spelling of a param value, so `true`, `"true"` and a form's `"true"` agree, and
/// `3`, `3.0` and `"3"` agree. Used by `show_if` (and mirrored by the UI).
pub fn value_key(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            // Whole while an f64 holds it exactly, as the validator and the UI's `String(n)` read it.
            Some(f) if f.fract() == 0.0 && f.abs() <= crate::validate::MAX_EXACT_F64_INT => {
                format!("{}", f as i64)
            }
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}
