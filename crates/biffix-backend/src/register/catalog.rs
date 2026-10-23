//! What a frozen version says it gives and takes, for the catalog: its title, results and main
//! inputs ([`catalog_of`]), and the catalog its form froze ([`frozen_catalog`]).

use biffix_api::CatalogEntry;

/// The pipeline's friendly title: its form's `title` (from `biffix.yaml`), trimmed; `None` when
/// blank or absent.
pub(crate) fn form_title(form_schema: &serde_json::Value) -> Option<String> {
    form_schema
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// A version's catalog entry: the one frozen with its form, its description falling back to
/// `summary` (the form's description, else the About page's first sentence). `None` when there is
/// nothing to show. Read from the form the list already loads, so the list costs no extra query.
pub(crate) fn catalog_of(
    form_schema: &serde_json::Value,
    summary: Option<&str>,
) -> Option<CatalogEntry> {
    let frozen = frozen_catalog(form_schema);
    let view = CatalogEntry {
        description: frozen.description.or_else(|| summary.map(str::to_string)),
        tags: frozen.tags,
        verified: frozen.verified,
        contact: frozen.contact.map(Into::into),
        results: catalog_results(form_schema),
        inputs: catalog_inputs(form_schema),
    };
    (view != CatalogEntry::default()).then_some(view)
}

/// At most this many inputs on a catalog card: the main ones, not every file a form takes.
const MAX_CATALOG_INPUTS: usize = 4;

/// The names of the key results a version declares (its form's `reports`), in order, each once
/// (two declarations may share a name). Lenient, like the catalog.
fn catalog_results(form_schema: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for report in form_schema.get("reports").and_then(|r| r.as_array()).into_iter().flatten() {
        let Some(name) = report.get("name").and_then(|n| n.as_str()).map(str::trim) else {
            continue;
        };
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// What a run takes, in plain words: each samplesheet field (`Samples (one row per sample)`) and
/// each required, shown file input (with the endings it accepts) the reference dropdown does not
/// fill, then that dropdown's label; in form order, each once, at most [`MAX_CATALOG_INPUTS`], the
/// reference always among them (the field inputs give up the last slot for it). Read leniently off
/// the frozen form: a typed read would drop a field an older form froze without some key.
fn catalog_inputs(form_schema: &serde_json::Value) -> Vec<String> {
    use biffix_workflow::formschema::humanize;
    let flag = |f: &serde_json::Value, key: &str| {
        f.get(key).and_then(serde_json::Value::as_bool) == Some(true)
    };
    // Fields the reference dropdown fills are said once, as the reference.
    let filled =
        form_schema.get("references").and_then(|r| r.get("fill")).and_then(|f| f.as_object());
    let mut inputs: Vec<String> = Vec::new();
    for field in form_schema.get("fields").and_then(|f| f.as_array()).into_iter().flatten() {
        let sheet = field.get("samplesheet").is_some_and(|s| !s.is_null());
        let file = flag(field, "is_path_input")
            && flag(field, "required")
            && !flag(field, "is_output")
            && !flag(field, "hidden");
        let by_reference = field
            .get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| filled.is_some_and(|f| f.contains_key(n)));
        if (!sheet && !file) || by_reference {
            continue;
        }
        let label = field
            .get("label")
            .and_then(|l| l.as_str())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .or_else(|| field.get("name").and_then(|n| n.as_str()).map(humanize));
        let Some(label) = label.filter(|l| !l.is_empty()) else {
            continue;
        };
        let suffixes: Vec<&str> = field
            .get("suffixes")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str())
            .collect();
        let text = if sheet {
            format!("{label} (one row per sample)")
        } else if suffixes.is_empty() {
            label
        } else {
            format!("{label} ({})", suffixes.join(", "))
        };
        if !inputs.contains(&text) {
            inputs.push(text);
        }
    }
    let reference = form_schema
        .get("references")
        .and_then(|r| r.get("label"))
        .and_then(|l| l.as_str())
        .map(str::trim)
        .filter(|l| !l.is_empty());
    match reference.filter(|r| !inputs.iter().any(|i| i == r)) {
        Some(reference) => {
            inputs.truncate(MAX_CATALOG_INPUTS - 1);
            inputs.push(reference.to_string());
        }
        None => inputs.truncate(MAX_CATALOG_INPUTS),
    }
    inputs
}

/// The catalog frozen with a version's form. Lenient: a form frozen before catalogs (or a
/// malformed one) has an empty one.
pub(crate) fn frozen_catalog(
    form_schema: &serde_json::Value,
) -> biffix_workflow::formschema::FormCatalog {
    use serde::Deserialize as _;
    form_schema
        .get("catalog")
        .and_then(|c| biffix_workflow::formschema::FormCatalog::deserialize(c).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalog_says_what_a_run_gives_and_takes() {
        let form = serde_json::json!({
            "fields": [
                {"name": "input", "samplesheet": {"columns": []}, "label": "Samples"},
                {"name": "fasta", "is_path_input": true, "required": true,
                 "suffixes": [".fa", ".fa.gz"]},
                {"name": "outdir", "is_path_input": true, "required": true, "is_output": true},
                {"name": "extra_bed", "is_path_input": true, "required": false},
                {"name": "hidden_ref", "is_path_input": true, "required": true, "hidden": true},
                {"name": "gtf", "is_path_input": true, "required": true},
                {"name": "min_reads", "required": true}
            ],
            "references": {"label": "Reference genome", "fill": {"gtf": "gtf"}, "required": []},
            "reports": [
                {"name": "QC report", "path": "multiqc.html"},
                {"name": "Per-sample QC", "path": "{sample}.html", "per_sample": true},
                {"name": "QC report", "path": "other.html"},
                {"name": "  ", "path": "blank.html"}
            ]
        });
        let catalog = catalog_of(&form, None).unwrap();
        assert_eq!(catalog.results, ["QC report", "Per-sample QC"]);
        assert_eq!(
            catalog.inputs,
            ["Samples (one row per sample)", "Fasta (.fa, .fa.gz)", "Reference genome"]
        );
        // A form frozen before any of it: nothing to say, and no catalog at all.
        assert_eq!(catalog_of(&serde_json::json!({"fields": []}), None), None);
    }

    #[test]
    fn a_catalog_lists_at_most_four_inputs() {
        let fields: Vec<_> = (0..6)
            .map(|i| serde_json::json!({"name": format!("in_{i}"), "is_path_input": true, "required": true}))
            .collect();
        let catalog = catalog_of(&serde_json::json!({ "fields": fields }), None).unwrap();
        assert_eq!(catalog.inputs, ["In 0", "In 1", "In 2", "In 3"]);
        // The reference is never the one cut.
        let form =
            serde_json::json!({ "fields": fields, "references": {"label": "Reference genome"} });
        let catalog = catalog_of(&form, None).unwrap();
        assert_eq!(catalog.inputs, ["In 0", "In 1", "In 2", "Reference genome"]);
        // A reference already listed (a field of that label) takes no slot of its own.
        let mut named = fields.clone();
        named[0]["label"] = "Reference genome".into();
        let form =
            serde_json::json!({ "fields": named, "references": {"label": "Reference genome"} });
        let catalog = catalog_of(&form, None).unwrap();
        assert_eq!(catalog.inputs, ["Reference genome", "In 1", "In 2", "In 3"]);
    }

    #[test]
    fn a_title_is_trimmed_and_blank_is_none() {
        assert_eq!(
            form_title(&serde_json::json!({"title": "  Bulk RNA-seq\n"})).as_deref(),
            Some("Bulk RNA-seq")
        );
        assert_eq!(form_title(&serde_json::json!({"title": " \t\n"})), None);
        assert_eq!(form_title(&serde_json::json!({"title": 3})), None);
        assert_eq!(form_title(&serde_json::json!({})), None);
    }
}
