//! Param validation: the pure, schema-level half of a submit.
//!
//! - [`validate_params`] checks the schema rules — `required` (a declared `default` satisfies it;
//!   a blank string does not), `enum` allow-lists, number ranges and integers, string `rules` patterns, and path inputs'
//!   shape — over the *active* fields only;
//! - [`inactive_fields`] / [`prune_inactive`] decide which fields a `show_if` gate hides, so their
//!   params never reach validation or the engine (a field on a [`show_if_cycles`] cycle is always
//!   shown);
//! - [`coerce_params`] turns a boolean's or a number's string spelling into the JSON value the
//!   engine expects, and [`default_nextflow_outdir`] points an unset Nextflow `outdir` at the run's
//!   output prefix;
//! - [`input_paths_to_check`] lists the input URIs (outputs skipped) the backend existence-checks.
//!
//! Nothing here touches AWS: the existence checks themselves run in the backend's submit path
//! (`input_candidates` and `check_inputs` in `crates/biffix-backend/src/runs/submit/mod.rs`), and
//! secret redaction lives in
//! `crates/biffix-backend/src/runs/submit/redaction.rs`.
//!
//! Lives in biffix-workflow so the CLI's pre-flight validation and the server validator are the
//! same function — the load-bearing "validation agreement" property. The UI's live validation is a
//! TypeScript port (`ui/src/lib/launch/validate.ts`) held to this one by the shared cases in
//! `xtask/src/cases/validate.rs`.

use serde_json::Value;

use std::collections::{BTreeMap, BTreeSet};

use crate::formschema::{value_key, FormField, WidgetHint};

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

/// The Nextflow param pipelines publish their results under (nf-core requires it).
pub const NEXTFLOW_OUTDIR_PARAM: &str = "outdir";

/// Default a Nextflow version's declared `outdir` param to `<output prefix>results` when the submit
/// left it unset, so a pipeline following the nf-core convention publishes into the run's S3 prefix
/// (where biffix registers its outputs) rather than the coordinator's disk, which dies with the
/// job. A caller-supplied value always wins, and a version that does not declare `outdir` is
/// untouched (an undeclared param would only earn a Nextflow warning). The server applies it before
/// validation, so a required `outdir` is satisfied and the anchor records the value the run used;
/// the CLI applies it (with a placeholder prefix) to the copy it pre-validates, so a submit the
/// server would accept is never rejected client-side.
pub fn default_nextflow_outdir(
    fields: &[FormField],
    params: &mut serde_json::Value,
    output_prefix: Option<&str>,
) {
    let Some(prefix) = output_prefix else { return };
    if !fields.iter().any(|f| f.name == NEXTFLOW_OUTDIR_PARAM) {
        return;
    }
    if params.is_null() {
        *params = serde_json::json!({});
    }
    let Some(map) = params.as_object_mut() else {
        return;
    };
    let unset = match map.get(NEXTFLOW_OUTDIR_PARAM) {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.trim().is_empty(),
        Some(_) => false,
    };
    if unset {
        let base = if prefix.ends_with('/') { prefix.to_string() } else { format!("{prefix}/") };
        map.insert(
            NEXTFLOW_OUTDIR_PARAM.to_string(),
            serde_json::Value::String(format!("{base}results")),
        );
    }
}

/// A boolean parameter: the registration adapter renders a JSON-Schema `boolean` as a two-choice
/// Enum with exactly the choices `true`/`false` (`formschema`). The field's recorded `json_type`
/// decides when present, so a `type: string` param whose choices happen to be `"true"`/`"false"`
/// stays a string; a form frozen before `json_type` was recorded falls back to the enum's shape.
pub fn is_boolean_choice(field: &FormField) -> bool {
    let shaped = field
        .enum_values
        .as_deref()
        .is_some_and(|v| v.len() == 2 && v.contains(&"true".into()) && v.contains(&"false".into()));
    shaped && field.json_type.as_deref().is_none_or(|t| t == "boolean")
}

/// A number field that takes whole numbers only: its recorded `json_type` is `integer`. `step` is a
/// hint for the input's increments (an overlay may set it), so it neither lifts nor adds the check;
/// a form frozen before `json_type` was recorded falls back to the integer marker `step: 1`.
fn is_integer(field: &FormField) -> bool {
    match field.json_type.as_deref() {
        Some(t) => t == "integer",
        None => field.step == Some(1.0),
    }
}

/// A field whose values are numbers: a Number widget, or a choice among numbers (an `integer` or
/// `number` enum).
fn is_numeric(field: &FormField) -> bool {
    field.widget == WidgetHint::Number
        || (field.enum_values.is_some()
            && matches!(field.json_type.as_deref(), Some("integer" | "number")))
}

/// The largest magnitude up to which an `f64` holds every whole number exactly (2^53).
pub(crate) const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0;

/// A number's string spelling as the JSON number the engine should receive, or `None` to leave it
/// as given (for [`validate_params`] to judge). A whole-number literal converts exactly or not at
/// all: one too big for 64 bits stays text, which [`validate_params`] accepts as a finite number,
/// so the engine receives it as a JSON string (as the UI's port does). Other text converts when it
/// reads as a finite `f64`, written as an integer while that is exact.
fn number_from_text(text: &str) -> Option<Value> {
    let t = text.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        let t = t.strip_prefix('+').unwrap_or(t);
        return t
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| t.parse::<u64>().map(Value::from))
            .ok();
    }
    let n = t.parse::<f64>().ok().filter(|n| n.is_finite())?;
    if n.fract() == 0.0 && n.abs() <= MAX_EXACT_F64_INT {
        return Some(Value::from(n as i64));
    }
    serde_json::Number::from_f64(n).map(Value::Number)
}

/// Turn a param given as the string spelling of its type into the JSON value the engine expects:
/// a boolean's `"true"`/`"false"` (a form's choice, a quoted YAML value) into a JSON boolean —
/// Nextflow (Groovy) treats any non-empty string, `"false"` included, as truthy — and a number's
/// `"8"` into `8`, as [`validate_params`] reads it. Other values are left untouched for
/// [`validate_params`] to judge; a non-object `params` is returned as-is.
pub fn coerce_params(fields: &[FormField], mut params: Value) -> Value {
    let Some(obj) = params.as_object_mut() else {
        return params;
    };
    for field in fields {
        let Some(v) = obj.get_mut(&field.name) else {
            continue;
        };
        if is_boolean_choice(field) {
            match v.as_str() {
                Some("true") => *v = Value::Bool(true),
                Some("false") => *v = Value::Bool(false),
                _ => {}
            }
        } else if is_numeric(field) {
            if let Some(n) = v.as_str().and_then(number_from_text) {
                *v = n;
            }
        }
    }
    params
}

/// The fields whose `show_if` conditions reach back to themselves. Registration warns that such a
/// field is always shown, and [`inactive_fields`] shows it.
pub fn show_if_cycles(fields: &[FormField]) -> BTreeSet<String> {
    let by_name: BTreeMap<&str, &FormField> = fields.iter().map(|f| (f.name.as_str(), f)).collect();
    let deps = |name: &str| -> Vec<&str> {
        by_name
            .get(name)
            .map(|f| f.show_if.iter().map(|c| c.field.as_str()).collect())
            .unwrap_or_default()
    };
    let mut cyclic = BTreeSet::new();
    for field in fields {
        let mut stack = deps(&field.name);
        let mut seen = BTreeSet::new();
        while let Some(next) = stack.pop() {
            if next == field.name {
                cyclic.insert(field.name.clone());
                break;
            }
            if seen.insert(next) {
                stack.extend(deps(next));
            }
        }
    }
    cyclic
}

/// The fields a form would hide for these params: a field is inactive when any `show_if` condition
/// fails, or names a field that is itself inactive. A condition compares the other field's value —
/// the given one, else its default — by [`value_key`]. A field on a `show_if` cycle is always
/// shown (registration warns), whatever the values; a field gated on one is judged as usual.
pub fn inactive_fields(fields: &[FormField], params: &Value) -> BTreeSet<String> {
    let by_name: BTreeMap<&str, &FormField> = fields.iter().map(|f| (f.name.as_str(), f)).collect();
    let cyclic = show_if_cycles(fields);
    let effective = |name: &str| -> String {
        match params.get(name) {
            Some(v) if !v.is_null() => value_key(v),
            _ => by_name
                .get(name)
                .and_then(|f| f.default.as_ref())
                .map(value_key)
                .unwrap_or_default(),
        }
    };
    // With the cyclic fields settled as shown, the rest of the `show_if` graph is acyclic, so each
    // memoized answer is final and the result does not depend on field order.
    fn active<'a>(
        name: &'a str,
        by_name: &BTreeMap<&'a str, &'a FormField>,
        cyclic: &BTreeSet<String>,
        effective: &dyn Fn(&str) -> String,
        memo: &mut BTreeMap<&'a str, bool>,
    ) -> bool {
        if let Some(known) = memo.get(name) {
            return *known;
        }
        let Some(field) = by_name.get(name) else {
            return true;
        };
        let shown = cyclic.contains(name)
            || field.show_if.iter().all(|c| {
                let dep = c.field.as_str();
                active(dep, by_name, cyclic, effective, memo) && {
                    let current = effective(dep);
                    c.values.iter().any(|v| value_key(v) == current)
                }
            });
        memo.insert(name, shown);
        shown
    }
    let mut memo = BTreeMap::new();
    fields
        .iter()
        .filter(|f| !active(&f.name, &by_name, &cyclic, &effective, &mut memo))
        .map(|f| f.name.clone())
        .collect()
}

/// Params with the inactive fields' values removed ([`prune_inactive`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Pruned {
    /// The params without the inactive fields' values.
    pub params: Value,
    /// The names whose values were removed, for the run's record.
    pub dropped: Vec<String>,
    /// Every inactive field, given or not, so a caller can drop other inputs for them (a
    /// samplesheet's rows) without recomputing the set.
    pub inactive: BTreeSet<String>,
}

/// Remove the params of inactive fields (a hidden UMI pattern never reaches the engine).
pub fn prune_inactive(fields: &[FormField], mut params: Value) -> Pruned {
    let inactive = inactive_fields(fields, &params);
    let mut dropped = Vec::new();
    if let Some(obj) = params.as_object_mut() {
        for name in &inactive {
            if obj.remove(name).is_some() {
                dropped.push(name.clone());
            }
        }
    }
    Pruned { params, dropped, inactive }
}

/// A number field's value as a number: a JSON number or a numeric string.
fn as_number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// Range, integer and pattern checks for a present, non-choice value.
fn check_value(field: &FormField, v: &Value) -> Option<String> {
    if field.widget == WidgetHint::Number {
        // `str::parse::<f64>` reads `nan` and `inf`, which no range check can bound.
        let Some(n) = as_number(v).filter(|n| n.is_finite()) else {
            return Some("must be a number".into());
        };
        if is_integer(field) && n.fract() != 0.0 {
            return Some("must be a whole number".into());
        }
        match (field.min, field.max) {
            (Some(lo), Some(hi)) if n < lo || n > hi => {
                return Some(format!("must be between {} and {}", fmt_num(lo), fmt_num(hi)))
            }
            (Some(lo), None) if n < lo => return Some(format!("must be at least {}", fmt_num(lo))),
            (None, Some(hi)) if n > hi => return Some(format!("must be at most {}", fmt_num(hi))),
            _ => {}
        }
        if let Some(lo) = field.exclusive_min.filter(|lo| n <= *lo) {
            return Some(format!("must be greater than {}", fmt_num(lo)));
        }
        if let Some(hi) = field.exclusive_max.filter(|hi| n >= *hi) {
            return Some(format!("must be less than {}", fmt_num(hi)));
        }
        if let Some(m) = field.multiple_of.filter(|m| *m > 0.0) {
            // Within a relative epsilon, so `0.3` is a multiple of `0.1` despite binary rounding.
            let q = n / m;
            if (q - q.round()).abs() > 1e-9 * q.abs().max(1.0) {
                return Some(format!("must be a multiple of {}", fmt_num(m)));
            }
        }
    }
    if field.rules.is_empty() {
        return None;
    }
    // A pattern is matched against a scalar as the server compares it (`8` as `8`); a list or an
    // object is never the text a pattern describes.
    let text = match v {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => value_key(v),
        _ => return Some("must be text".into()),
    };
    for rule in &field.rules {
        // An author pattern Rust cannot compile is not the submitter's fault; skip it.
        if let Ok(re) = regex::Regex::new(&rule.pattern) {
            if !re.is_match(&text) {
                return Some(rule.message.clone());
            }
        }
    }
    None
}

fn fmt_num(n: f64) -> String {
    value_key(&serde_json::json!(n))
}

/// A path input's value: one string path, or (for a `multiple` field) a non-empty list of them,
/// each checked against the field's rules.
fn check_path(field: &FormField, v: &Value) -> Option<String> {
    match v {
        Value::String(_) => check_value(field, v),
        Value::Array(items) if field.multiple => {
            if items.is_empty() {
                return Some("must list at least one path".into());
            }
            if !items.iter().all(Value::is_string) {
                return Some("must be a list of string paths".into());
            }
            items.iter().enumerate().find_map(|(i, item)| {
                check_value(field, item).map(|m| format!("path {}: {m}", i + 1))
            })
        }
        // Without this, a non-string (number/array/object) passes validation AND is skipped by the
        // existence check, so a malformed required path flows through entirely unvalidated.
        _ => Some("must be a string path".into()),
    }
}

/// Whether `value` (a string given for `field`) is blank where the field needs a value: `field` is
/// required, has no default, and `value` is empty or whitespace. [`validate_params`] reports it as
/// `required` and [`input_paths_to_check`] leaves it out.
fn is_blank_required(field: &FormField, value: &str) -> bool {
    needs_value(field) && value.trim().is_empty()
}

/// Whether `field` needs a value from the submitter: it is required and has no default.
fn needs_value(field: &FormField) -> bool {
    field.required && field.default.is_none()
}

/// Validate resolved params against the normalized FormSchema fields. Returns all errors (not just
/// the first), so the UI/footer can name every failing field. Operating directly on `FormField` —
/// the single normalized field model produced by the registration adapter — means the CLI's
/// pre-flight check and the server validator reason over identical data.
pub fn validate_params(fields: &[FormField], params: &Value) -> Vec<ValidationError> {
    let Some(obj) = params.as_object() else {
        return vec![ValidationError {
            field: "$".into(),
            message: "parameters must be a JSON object".into(),
        }];
    };
    let mut errors = Vec::new();
    let inactive = inactive_fields(fields, params);

    for field in fields.iter().filter(|f| !inactive.contains(&f.name)) {
        let message = match obj.get(&field.name) {
            // The engine runs an omitted param with its declared default, so a default satisfies
            // `required`.
            None | Some(Value::Null) => needs_value(field).then(|| "required".to_string()),
            // A blank string gives a required param no value. With a default it is left to the
            // checks below, like any other value.
            Some(Value::String(s)) if is_blank_required(field, s) => Some("required".into()),
            Some(v) if is_boolean_choice(field) => {
                // A boolean accepts a JSON boolean or its string spelling (coerced before launch by
                // `coerce_params`).
                let ok = v.is_boolean() || matches!(v.as_str(), Some("true" | "false"));
                (!ok).then(|| "must be true or false".to_string())
            }
            Some(v) => match &field.enum_values {
                // Compared by `value_key`, so an integer choice `2` matches `2`, `2.0` and `"2"`; a
                // list or an object is never one of the choices.
                Some(allowed) => {
                    let ok = !v.is_array() && !v.is_object() && allowed.contains(&value_key(v));
                    (!ok).then(|| format!("not one of {}", allowed.join(", ")))
                }
                None if field.is_path_input => check_path(field, v),
                None => check_value(field, v),
            },
        };
        if let Some(message) = message {
            errors.push(ValidationError { field: field.name.clone(), message });
        }
    }
    errors
}

/// An input path to existence-check before launch ([`input_paths_to_check`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputPath {
    pub field: String,
    pub uri: String,
    /// A prefix input (`WidgetHint::S3Dir`): checked by a ListBucket prefix probe rather than a
    /// HEAD on the URI.
    pub is_prefix: bool,
}

/// The input paths to existence-check, in field order: each path input's string value, or each
/// element of a `multiple` field's list, skipping outputs, which need not exist yet. A value
/// of another shape, or a blank one for a required field without a default, is left out;
/// [`validate_params`] rejects it.
pub fn input_paths_to_check(fields: &[FormField], params: &Value) -> Vec<InputPath> {
    let mut paths = Vec::new();
    for field in fields.iter().filter(|f| f.is_path_input && !f.is_output) {
        let uris: Vec<&str> = match params.get(&field.name) {
            Some(Value::String(s)) if is_blank_required(field, s) => continue,
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(items)) if field.multiple => {
                items.iter().filter_map(Value::as_str).collect()
            }
            _ => continue,
        };
        paths.extend(uris.into_iter().map(|uri| InputPath {
            field: field.name.clone(),
            uri: uri.to_string(),
            is_prefix: field.widget == WidgetHint::S3Dir,
        }));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formschema::ShowIf;
    use serde_json::json;

    fn field(name: &str, widget: WidgetHint) -> FormField {
        FormField { name: name.into(), widget, ..FormField::default() }
    }

    fn gated(name: &str, on: &str, value: &str) -> FormField {
        FormField {
            show_if: vec![ShowIf { field: on.into(), values: vec![json!(value)] }],
            ..field(name, WidgetHint::Text)
        }
    }

    #[test]
    fn a_show_if_cycle_is_shown_in_any_field_order() {
        let (a, b, c) = (gated("a", "b", "x"), gated("b", "c", "y"), gated("c", "a", "z"));
        let params = json!({"a": "1", "b": "2", "c": "3"});
        for order in [
            vec![a.clone(), b.clone(), c.clone()],
            vec![c.clone(), b.clone(), a.clone()],
            vec![b.clone(), a.clone(), c.clone()],
        ] {
            assert!(inactive_fields(&order, &params).is_empty());
            let pruned = prune_inactive(&order, params.clone());
            assert_eq!(pruned.params, params);
            assert!(pruned.dropped.is_empty());
        }
        // A field gated on a cyclic one is still judged by its condition.
        let fields = vec![a, b, c, gated("d", "a", "1"), gated("e", "a", "nope")];
        assert_eq!(inactive_fields(&fields, &params), BTreeSet::from(["e".to_string()]));
    }

    #[test]
    fn prune_returns_the_inactive_set_it_used() {
        let fields = vec![field("mode", WidgetHint::Text), gated("depth", "mode", "slow")];
        let pruned = prune_inactive(&fields, json!({"mode": "fast", "depth": 3}));
        assert_eq!(pruned.params, json!({"mode": "fast"}));
        assert_eq!(pruned.dropped, vec!["depth".to_string()]);
        assert_eq!(pruned.inactive, BTreeSet::from(["depth".to_string()]));
    }

    #[test]
    fn coerce_turns_number_strings_into_exact_numbers() {
        let fields = vec![
            field("n", WidgetHint::Number),
            field("x", WidgetHint::Number),
            field("big", WidgetHint::Number),
            field("huge", WidgetHint::Number),
            field("wide", WidgetHint::Number),
            field("e", WidgetHint::Number),
            field("bad", WidgetHint::Number),
            field("nan", WidgetHint::Number),
            field("text", WidgetHint::Text),
        ];
        let got = coerce_params(
            &fields,
            json!({
                "n": " 8 ", "x": "2.5", "big": "9007199254740993", "huge": "99999999999999999999",
                "wide": "1e300", "e": "1e1", "bad": "0x10", "nan": "NaN", "text": "8",
            }),
        );
        assert_eq!(got["n"], json!(8));
        assert_eq!(got["x"], json!(2.5));
        // A whole number too large for an f64 converts exactly, or stays text.
        assert_eq!(got["big"], json!(9_007_199_254_740_993_i64));
        assert_eq!(got["huge"], json!("99999999999999999999"));
        assert_eq!(got["wide"], json!(1e300));
        assert_eq!(got["e"], json!(10));
        // What validation rejects is left for it to name; a text field keeps its string.
        assert_eq!(got["bad"], json!("0x10"));
        assert_eq!(got["nan"], json!("NaN"));
        assert_eq!(got["text"], json!("8"));
    }

    #[test]
    fn coerce_follows_the_declared_type() {
        let choice = |name: &str, json_type: &str, values: &[&str]| FormField {
            enum_values: Some(values.iter().map(std::string::ToString::to_string).collect()),
            json_type: Some(json_type.into()),
            ..field(name, WidgetHint::Enum)
        };
        let fields = vec![
            choice("flag", "boolean", &["true", "false"]),
            choice("word", "string", &["true", "false"]),
            choice("level", "integer", &["1", "2"]),
            // A form frozen before `json_type` was recorded: a true/false choice is a boolean.
            FormField { json_type: None, ..choice("legacy", "", &["true", "false"]) },
        ];
        let got = coerce_params(
            &fields,
            json!({"flag": "true", "word": "true", "level": "2", "legacy": "false"}),
        );
        assert_eq!(got, json!({"flag": true, "word": "true", "level": 2, "legacy": false}));
        assert!(is_boolean_choice(&fields[0]));
        assert!(!is_boolean_choice(&fields[1]));
        assert!(is_boolean_choice(&fields[3]));
    }

    #[test]
    fn input_paths_expand_a_multiple_field_only() {
        let path = |name: &str, widget: WidgetHint, multiple: bool| FormField {
            is_path_input: true,
            multiple,
            ..field(name, widget)
        };
        let fields = vec![
            path("reads", WidgetHint::S3File, true),
            path("single", WidgetHint::S3File, false),
            path("refs", WidgetHint::S3Dir, false),
            FormField { is_output: true, ..path("outdir", WidgetHint::S3Dir, false) },
        ];
        let params = json!({
            "reads": ["s3://b/a.fq", "s3://b/b.fq", 3],
            "single": ["s3://b/c.fq"],
            "refs": "s3://b/refs/",
            "outdir": "s3://b/out/",
        });
        let got: Vec<(String, String, bool)> = input_paths_to_check(&fields, &params)
            .into_iter()
            .map(|p| (p.field, p.uri, p.is_prefix))
            .collect();
        assert_eq!(
            got,
            vec![
                ("reads".into(), "s3://b/a.fq".into(), false),
                ("reads".into(), "s3://b/b.fq".into(), false),
                ("refs".into(), "s3://b/refs/".into(), true),
            ]
        );
    }

    #[test]
    fn a_blank_required_path_is_required_not_checked() {
        let path = |name: &str, required: bool| FormField {
            is_path_input: true,
            required,
            ..field(name, WidgetHint::S3File)
        };
        let fields = vec![path("reads", true), path("panel", false)];
        let params = json!({"reads": "  ", "panel": ""});
        assert_eq!(
            validate_params(&fields, &params),
            vec![ValidationError { field: "reads".into(), message: "required".into() }]
        );
        // The optional blank path is still checked, so the backend rejects it as malformed.
        let uris: Vec<String> =
            input_paths_to_check(&fields, &params).into_iter().map(|p| p.field).collect();
        assert_eq!(uris, ["panel"]);
    }
}
