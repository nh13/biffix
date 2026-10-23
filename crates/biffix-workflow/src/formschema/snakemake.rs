//! The Snakemake adapter: a JSON-Schema-shaped config schema plus manifest hints becomes a
//! FormSchema.

use serde_json::Value;

use crate::formschema::fields::{
    fallback_sections, field_from, warn_unmatched_hints, OTHER_SECTION, REQUIRED_SECTION,
};
use crate::formschema::{json_string_array, FormSchema, SchemaOrder};
use crate::manifest::BiffixManifest;

// Named only by the adapter's doc link.
#[cfg(doc)]
use crate::formschema::parse_schema_text;

/// Adapt a JSON-Schema-shaped config (Snakemake) + manifest hints → FormSchema. A config schema has
/// no groups, so required params form an *Inputs* section and the rest an advanced *Other options*
/// section, each in `order` (from [`parse_schema_text`]). Lenient.
pub fn from_snakemake_config_ordered(
    schema: &Value,
    order: &SchemaOrder,
    manifest: &BiffixManifest,
) -> FormSchema {
    let mut out = FormSchema::default();

    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        out.warnings.push("config schema has no `properties`; no fields extracted".into());
        return out;
    };
    let required = json_string_array(schema.get("required")).unwrap_or_default();
    let mut names: Vec<&String> = props.keys().collect();
    names.sort_by_key(|n| order.property_rank("", n));

    for name in names {
        // Lenient: a field that isn't an object is skipped with a warning, not fatal.
        let Some(obj) = props[name].as_object() else {
            out.warnings.push(format!("field `{name}` is not an object; skipped"));
            continue;
        };
        let is_required = required.contains(name);
        let field = field_from(
            name,
            obj,
            manifest.params.get(name),
            is_required,
            if is_required { REQUIRED_SECTION } else { OTHER_SECTION },
            false,
            &mut out.warnings,
        );
        out.fields.push(field);
    }
    // Required fields first (their section is shown first), each group in schema order.
    out.fields.sort_by_key(|f| !f.required);
    out.sections = fallback_sections(&out.fields);
    warn_unmatched_hints(&mut out, manifest);
    out
}
