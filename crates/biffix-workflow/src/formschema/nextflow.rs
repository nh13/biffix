//! The Nextflow adapter: an nf-schema `nextflow_schema.json` plus manifest hints becomes a
//! FormSchema.

use serde_json::Value;

use crate::formschema::fields::{
    fallback_sections, field_from, warn_unmatched_hints, OTHER_SECTION, REQUIRED_SECTION,
};
use crate::formschema::{humanize, json_string_array, FormSchema, FormSection, SchemaOrder};
use crate::manifest::BiffixManifest;

// Named only by the adapter's doc link.
#[cfg(doc)]
use crate::formschema::parse_schema_text;

/// Adapt an nf-schema `nextflow_schema.json` + manifest hints → FormSchema. Params live under
/// grouped `definitions` (or `$defs` in nf-schema v2), each an object schema with its own
/// `properties` + `required` and a `title`, and/or directly under a top-level `properties`. Each
/// group becomes a section, in `order` (from [`parse_schema_text`]); a group with no required param
/// is advanced. A samplesheet param (one carrying a row `schema` file) gets its columns from that
/// file, read with `read_file` from the workflow repo; a file that cannot be read or parsed is a
/// warning, and the field then takes a samplesheet path only. Lenient: a malformed entry is skipped
/// with a warning, never a hard abort.
pub fn from_nextflow_schema_ordered(
    schema: &Value,
    order: &SchemaOrder,
    manifest: &BiffixManifest,
    read_file: &dyn Fn(&str) -> Result<String, String>,
) -> FormSchema {
    let mut out = FormSchema::default();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Each "group" is an object schema exposing `properties` (+ optionally `required`): the grouped
    // definitions/$defs, plus the top-level schema itself (key "") for any ungrouped params.
    let mut groups: Vec<(String, &serde_json::Map<String, Value>)> = Vec::new();
    for key in ["definitions", "$defs"] {
        if let Some(defs) = schema.get(key).and_then(|d| d.as_object()) {
            for (gkey, gspec) in defs {
                if let Some(gobj) = gspec.as_object() {
                    groups.push((gkey.clone(), gobj));
                }
            }
        }
    }
    groups.sort_by_key(|(gkey, _)| order.group_rank(gkey));
    if let Some(top) = schema.as_object() {
        if top.contains_key("properties") {
            groups.push((String::new(), top));
        }
    }
    if groups.is_empty() {
        out.warnings.push(
            "nextflow schema has no `definitions`/`$defs`/`properties`; no fields extracted".into(),
        );
        return out;
    }

    for (gkey, group) in groups {
        let required = json_string_array(group.get("required")).unwrap_or_default();
        let Some(props) = group.get("properties").and_then(|p| p.as_object()) else {
            continue;
        };
        let mut names: Vec<&String> = props.keys().collect();
        names.sort_by_key(|n| order.property_rank(&gkey, n));
        let mut group_fields = Vec::new();
        for name in names {
            let Some(obj) = props[name].as_object() else {
                out.warnings.push(format!("param `{name}` is not an object; skipped"));
                continue;
            };
            // A param can appear in only one place; the first wins (defs before top-level).
            if !seen.insert(name.clone()) {
                continue;
            }
            let is_required = required.contains(name);
            let section = if !gkey.is_empty() {
                gkey.as_str()
            } else if is_required {
                REQUIRED_SECTION
            } else {
                OTHER_SECTION
            };
            let mut field = field_from(
                name,
                obj,
                manifest.params.get(name),
                is_required,
                section,
                true,
                &mut out.warnings,
            );
            if let Some(path) = obj.get("schema").and_then(|s| s.as_str()) {
                field.samplesheet = row_schema(name, path, read_file, &mut out.warnings);
            }
            group_fields.push(field);
        }
        if group_fields.is_empty() {
            continue;
        }
        if gkey.is_empty() {
            out.sections.extend(fallback_sections(&group_fields));
        } else {
            let text = |key: &str| group.get(key).and_then(|v| v.as_str()).map(String::from);
            out.sections.push(FormSection {
                id: gkey.clone(),
                title: text("title").unwrap_or_else(|| humanize(&gkey)),
                description: text("description"),
                advanced: !group_fields.iter().any(|f| f.required),
            });
        }
        out.fields.extend(group_fields);
    }
    warn_unmatched_hints(&mut out, manifest);
    out
}

/// A samplesheet param's columns, from its row `schema` file in the workflow repo; `None` (with a
/// warning) when the file cannot be read or declares no columns.
fn row_schema(
    name: &str,
    path: &str,
    read_file: &dyn Fn(&str) -> Result<String, String>,
    warnings: &mut Vec<String>,
) -> Option<crate::samples::SheetSpec> {
    match read_file(path).and_then(|t| crate::samples::spec_from_row_schema_text(&t)) {
        Ok(Some(spec)) => Some(spec),
        Ok(None) => {
            warnings
                .push(format!("`{name}`: {path} has no items.properties; no samplesheet columns"));
            None
        }
        Err(e) => {
            warnings.push(format!("`{name}`: cannot read samplesheet schema {path}: {e}"));
            None
        }
    }
}
