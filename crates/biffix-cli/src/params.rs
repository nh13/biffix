//! Submit parameter assembly: values from a file plus repeatable inline `--param k=v`, with inline
//! overriding the file, on top of what a `--resume-from` run inherits from
//! the run it resumes. Kept pure so the merge is property-tested (inline always wins,
//! no invented keys).

use biffix_workflow::formschema::{FormField, WidgetHint, REDACTED_SECRET_PLACEHOLDER};
use biffix_workflow::validate::is_boolean_choice;
use serde_json::{Map, Value};

use crate::error::{CliError, CliResult};

/// Parse `k=v` inline overrides, typing each value by the form field it names, so a text value is
/// never silently changed by reading it as JSON (`release=1.10` stays `"1.10"`, a long numeric id
/// keeps its digits, `label=null` stays `"null"`):
///
/// - a number (a Number widget, or a choice among numbers) or a boolean choice takes the value as
///   JSON when it reads as a number or a boolean respectively;
/// - a `multiple` field, or one whose JSON type is `array` or `object`, takes it as JSON when it
///   reads as that shape (`xs=["a","b"]`);
/// - every other declared field (text, a path, a choice among strings) keeps the raw string;
/// - an undeclared key (a schema-less form) is read as JSON when it parses (so `n=3`,
///   `flag=true`, `xs=[1,2]` are typed), and kept as a string otherwise.
///
/// A value that does not read as its field's type is kept as the string given, for validation to
/// report (the server turns a number's or a boolean's string spelling into the value itself).
pub(crate) fn parse_inline_for(
    pairs: &[String],
    fields: &[FormField],
) -> CliResult<Map<String, Value>> {
    let mut out = Map::new();
    // A refusal names the argument by position, never its text: a value may be a secret.
    for (i, pair) in pairs.iter().enumerate() {
        let (key, raw) = pair.split_once('=').ok_or_else(|| {
            CliError::InvalidInput(format!(
                "--param must be key=value; --param #{} has no `=`",
                i + 1
            ))
        })?;
        if key.is_empty() {
            return Err(CliError::InvalidInput(format!("--param #{} has an empty key", i + 1)));
        }
        let value = match fields.iter().find(|f| f.name == key) {
            Some(field) => typed_value(field, raw),
            None => serde_json::from_str::<Value>(raw)
                .unwrap_or_else(|_| Value::String(raw.to_string())),
        };
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

/// `raw` as `field` takes it (see [`parse_inline_for`]).
fn typed_value(field: &FormField, raw: &str) -> Value {
    serde_json::from_str::<Value>(raw)
        .ok()
        .filter(|v| takes_typed(field, v))
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

/// Whether `field` takes `value` as the typed value it is, rather than as text.
fn takes_typed(field: &FormField, value: &Value) -> bool {
    let numeric = field.widget == WidgetHint::Number
        || (field.enum_values.is_some()
            && matches!(field.json_type.as_deref(), Some("integer" | "number")));
    let structured =
        field.multiple || matches!(field.json_type.as_deref(), Some("array" | "object"));
    (structured && (value.is_array() || value.is_object()))
        || (numeric && value.is_number())
        || (is_boolean_choice(field) && value.is_boolean())
}

/// The params a `--params` file gives as a number or a boolean although their field takes text:
/// YAML has already read `release: 1.10` as the number 1.1, and the text it was is gone, so the
/// caller warns rather than guess it back.
pub(crate) fn text_given_untyped(file: &Map<String, Value>, fields: &[FormField]) -> Vec<String> {
    fields
        .iter()
        .filter(|f| {
            file.get(&f.name)
                .is_some_and(|v| (v.is_number() || v.is_boolean()) && !takes_typed(f, v))
        })
        .map(|f| f.name.clone())
        .collect()
}

/// Load a params file (YAML or JSON) into a JSON object. Absent path ⇒ empty object.
pub(crate) fn load_file(path: Option<&std::path::Path>) -> CliResult<Map<String, Value>> {
    let Some(path) = path else {
        return Ok(Map::new());
    };
    let text = std::fs::read_to_string(path).map_err(|e| {
        CliError::InvalidInput(format!("cannot read params file {}: {e}", path.display()))
    })?;
    // JSON first (YAML's JSON is not quite JSON: a surrogate-pair escape, say), then YAML. A file
    // named `.json` is JSON, so its error is the JSON one.
    let json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) if json => {
            return Err(CliError::InvalidInput(format!(
                "invalid params file {}: {e}",
                path.display()
            )))
        }
        Err(_) => serde_norway::from_str(&text).map_err(|e| {
            CliError::InvalidInput(format!("invalid params file {}: {e}", path.display()))
        })?,
    };
    match value {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        _ => Err(CliError::InvalidInput(format!(
            "params file {} must be a mapping of parameters",
            path.display()
        ))),
    }
}

/// A run's params as `runs submit` assembles them: what a resumed run `inherited`, overridden by
/// the `--params` `file`, overridden by the `--param` `inline` pairs (typed by the form's
/// `fields`). The result's keys are exactly the union of the three (no invented keys).
pub fn assemble(
    inherited: Map<String, Value>,
    file: Map<String, Value>,
    inline: &[String],
    fields: &[FormField],
) -> CliResult<Map<String, Value>> {
    let mut params = inherited;
    params.extend(file);
    params.extend(parse_inline_for(inline, fields)?);
    Ok(params)
}

/// What a resumed run inherits from the run it resumes (`--resume-from`).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Inherited {
    /// The prior run's params to use as the base, before any `--params`/`--param`.
    pub(crate) params: Map<String, Value>,
    /// Secret fields not carried over: the recorded copy holds only the redaction placeholder, so a
    /// secret must be supplied again.
    pub(crate) skipped_secrets: Vec<String>,
    /// Params the new version's form does not declare (e.g. removed in the new version).
    pub(crate) skipped_undeclared: Vec<String>,
}

/// The params a resumed run inherits: the prior run's recorded (secret-redacted) params, minus any
/// secret — by the new form's `is_secret`, or by the redaction placeholder itself, in case the
/// field was secret in the prior version only — and minus any the new form does not declare
/// (unless the form declares nothing, as for a schema-less workflow). `prior` that is not an object
/// (a run recorded without params) inherits nothing.
pub(crate) fn inherit(prior: &Value, fields: &[FormField]) -> Inherited {
    let mut out = Inherited::default();
    let Some(prior) = prior.as_object() else {
        return out;
    };
    let declared = |name: &str| fields.is_empty() || fields.iter().any(|f| f.name == name);
    let secret = |name: &str| fields.iter().any(|f| f.name == name && f.is_secret);
    for (name, value) in prior {
        if secret(name) || value.as_str() == Some(REDACTED_SECRET_PLACEHOLDER) {
            out.skipped_secrets.push(name.clone());
        } else if !declared(name) {
            out.skipped_undeclared.push(name.clone());
        } else {
            out.params.insert(name.clone(), value.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_workflow::formschema::WidgetHint;

    fn field(name: &str, is_secret: bool) -> FormField {
        FormField {
            name: name.into(),
            widget: WidgetHint::Text,
            required: false,
            description: None,
            enum_values: None,
            is_output: false,
            is_secret,
            is_path_input: false,
            ..Default::default()
        }
    }

    #[test]
    fn inherit_keeps_declared_params_and_never_a_secret() {
        let prior = serde_json::json!({
            "reads": "s3://b/in.fq",
            "threads": 4,
            "api_key": REDACTED_SECRET_PLACEHOLDER,
            // Secret in the prior version only: caught by its placeholder value.
            "token": REDACTED_SECRET_PLACEHOLDER,
            "dropped": true,
        });
        let fields = [
            field("reads", false),
            field("threads", false),
            field("api_key", true),
            field("token", false),
        ];
        let got = inherit(&prior, &fields);
        assert_eq!(
            Value::Object(got.params),
            serde_json::json!({"reads": "s3://b/in.fq", "threads": 4})
        );
        assert_eq!(got.skipped_secrets, vec!["api_key", "token"]);
        assert_eq!(got.skipped_undeclared, vec!["dropped"]);
    }

    #[test]
    fn inherit_keeps_everything_for_a_schema_less_form_and_nothing_from_no_params() {
        let prior = serde_json::json!({"greeting": "hi", "fail_second": "true"});
        assert_eq!(Value::Object(inherit(&prior, &[]).params), prior);
        assert_eq!(inherit(&Value::Null, &[field("a", false)]), Inherited::default());
    }

    #[test]
    fn inline_overrides_file_and_types_values() {
        let file = load_from_str("reads: s3://a\nthreads: 1\n");
        let inline = ["threads=8".into(), "reads=s3://b".into()];
        let merged = assemble(Map::new(), file, &inline, &[]).unwrap();
        assert_eq!(merged["threads"], serde_json::json!(8)); // typed as number, inline wins
        assert_eq!(merged["reads"], serde_json::json!("s3://b")); // inline wins
    }

    #[test]
    fn result_keys_are_the_union_no_invented_keys() {
        let file = load_from_str("a: 1\nb: 2\n");
        let inline = ["b=3".into(), "c=4".into()];
        let merged = assemble(Map::new(), file, &inline, &[]).unwrap();
        let keys: std::collections::BTreeSet<_> = merged.keys().cloned().collect();
        assert_eq!(keys, ["a", "b", "c"].iter().map(std::string::ToString::to_string).collect());
    }

    fn typed(name: &str, widget: WidgetHint) -> FormField {
        FormField { widget, ..field(name, false) }
    }

    #[test]
    fn inline_values_are_typed_by_their_field() {
        let choice = |name: &str, values: &[&str], json_type: &str| FormField {
            enum_values: Some(values.iter().map(std::string::ToString::to_string).collect()),
            json_type: Some(json_type.into()),
            ..typed(name, WidgetHint::Enum)
        };
        let fields = [
            typed("release", WidgetHint::Text),
            typed("reads", WidgetHint::S3File),
            typed("threads", WidgetHint::Number),
            choice("flag", &["true", "false"], "boolean"),
            choice("mode", &["1.10", "2"], "string"),
            choice("level", &["1", "2"], "integer"),
            FormField { multiple: true, ..typed("inputs", WidgetHint::S3File) },
        ];
        let pairs: Vec<String> = [
            "release=1.10",
            "reads=12345678901234567890123",
            "threads=8",
            "flag=true",
            "mode=1.10",
            "level=2",
            "inputs=[\"s3://b/a\",\"s3://b/b\"]",
            "undeclared=1e3",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
        let got = Value::Object(parse_inline_for(&pairs, &fields).unwrap());
        assert_eq!(
            got,
            serde_json::json!({
                "release": "1.10",
                "reads": "12345678901234567890123",
                "threads": 8,
                "flag": true,
                "mode": "1.10",
                "level": 2,
                "inputs": ["s3://b/a", "s3://b/b"],
                "undeclared": 1000.0,
            })
        );
        // A value that does not read as its field's type stays the string given, for validation
        // to judge; `null` never silently unsets a declared field.
        let odd: Vec<String> = ["threads=null", "flag=yes", "inputs=s3://b/one", "release=null"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(
            Value::Object(parse_inline_for(&odd, &fields).unwrap()),
            serde_json::json!({
                "threads": "null", "flag": "yes", "inputs": "s3://b/one", "release": "null",
            })
        );
    }

    #[test]
    fn rejects_malformed_inline_without_echoing_it() {
        let refused = |pairs: &[&str], needle: &str| {
            let pairs: Vec<String> = pairs.iter().map(std::string::ToString::to_string).collect();
            let err = parse_inline_for(&pairs, &[]).unwrap_err();
            assert!(
                matches!(&err, CliError::InvalidInput(m)
                    if m.contains(needle) && !m.contains("s3cret")),
                "{needle}: {err:?}"
            );
        };
        refused(&["a=1", "s3cret"], "--param #2 has no `=`");
        refused(&["=s3cret"], "--param #1 has an empty key");
    }

    #[test]
    fn a_file_value_that_lost_its_text_is_named() {
        let fields = [
            typed("release", WidgetHint::Text),
            typed("reads", WidgetHint::S3File),
            typed("threads", WidgetHint::Number),
            FormField {
                enum_values: Some(vec!["true".into(), "false".into()]),
                json_type: Some("boolean".into()),
                ..typed("flag", WidgetHint::Enum)
            },
        ];
        let file = load_from_str(
            "release: 1.10\nreads: s3://b/r.fq\nthreads: 4\nflag: true\nlabel: yes\nquoted: '1.10'\n",
        );
        assert_eq!(text_given_untyped(&file, &fields), ["release"]);
    }

    #[test]
    fn a_params_file_is_json_or_yaml_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            load_file(Some(&path))
        };
        let refused = |r: CliResult<Map<String, Value>>, needle: &str| {
            assert!(
                matches!(&r, Err(CliError::InvalidInput(m)) if m.contains(needle)),
                "{needle}: {r:?}"
            );
        };
        assert_eq!(load_file(None).unwrap(), Map::new());
        assert_eq!(
            Value::Object(file("p.yaml", "reads: s3://b/r.fq\nthreads: 4\n").unwrap()),
            serde_json::json!({"reads": "s3://b/r.fq", "threads": 4})
        );
        // JSON as JSON reads it: a surrogate-pair escape is one character.
        assert_eq!(
            Value::Object(file("p.json", r#"{"label": "\ud83d\ude00"}"#).unwrap()),
            serde_json::json!({"label": "\u{1F600}"})
        );
        assert_eq!(file("empty.yaml", "").unwrap(), Map::new());
        refused(file("list.yaml", "[1, 2]\n"), "must be a mapping");
        refused(file("scalar.json", "3"), "must be a mapping");
        refused(file("bad.json", "{\"a\": "), "invalid params file");
        refused(load_file(Some(&dir.path().join("missing.yaml"))), "cannot read params file");
    }

    fn load_from_str(yaml: &str) -> Map<String, Value> {
        match serde_norway::from_str::<Value>(yaml).unwrap() {
            Value::Object(m) => m,
            _ => panic!("not a map"),
        }
    }
}
