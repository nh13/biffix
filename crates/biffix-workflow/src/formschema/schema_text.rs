//! Parsing a schema file's text while keeping the key order `serde_json::Value` loses
//! ([`SchemaOrder`]).

use serde_json::Value;

/// The key order of a parsed schema's maps, which `serde_json::Value` loses (its maps sort).
/// Recovered from an order-preserving YAML parse of the same text by [`parse_schema_text`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SchemaOrder {
    /// nf-schema group keys, in `allOf` reference order, else file order.
    pub groups: Vec<String>,
    /// Property keys per group key; `""` is the top-level `properties`.
    pub properties: std::collections::BTreeMap<String, Vec<String>>,
}

impl SchemaOrder {
    pub(super) fn property_rank(&self, group: &str, name: &str) -> usize {
        self.properties
            .get(group)
            .and_then(|names| names.iter().position(|n| n == name))
            .unwrap_or(usize::MAX)
    }

    pub(super) fn group_rank(&self, group: &str) -> usize {
        self.groups.iter().position(|g| g == group).unwrap_or(usize::MAX)
    }
}

/// Parse a schema file's text (JSON or YAML) into a JSON value plus the key order the JSON value
/// cannot keep.
pub fn parse_schema_text(text: &str) -> Result<(Value, SchemaOrder), String> {
    let yaml: serde_norway::Value = serde_norway::from_str(text).map_err(|e| e.to_string())?;
    let mut order = SchemaOrder::default();
    order.properties.insert(String::new(), mapping_keys(yaml.get("properties")));
    for defs_key in ["definitions", "$defs"] {
        if let Some(defs) = yaml.get(defs_key).and_then(|d| d.as_mapping()) {
            for (k, v) in defs {
                if let Some(k) = k.as_str() {
                    order.groups.push(k.to_string());
                    order.properties.insert(k.to_string(), mapping_keys(v.get("properties")));
                }
            }
        }
    }
    // nf-core lists its groups in `allOf: [{$ref: "#/$defs/<group>"}]`, the order the pipeline
    // means them to be read in; file order is the fallback for groups it does not list.
    let referenced: Vec<String> = yaml
        .get("allOf")
        .and_then(|a| a.as_sequence())
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("$ref").and_then(|r| r.as_str()))
        .map(|r| crate::text::basename(r).to_string())
        .filter(|g| order.groups.contains(g))
        .collect();
    if !referenced.is_empty() {
        let rest: Vec<String> =
            order.groups.iter().filter(|g| !referenced.contains(g)).cloned().collect();
        order.groups = referenced.into_iter().chain(rest).collect();
    }
    let value: Value = serde_norway::from_value(yaml).map_err(|e| e.to_string())?;
    Ok((value, order))
}

/// The string keys of a YAML mapping, in file order (none when `v` is not a mapping).
pub(crate) fn mapping_keys(v: Option<&serde_norway::Value>) -> Vec<String> {
    v.and_then(|v| v.as_mapping())
        .map(|m| m.keys().filter_map(|k| k.as_str().map(String::from)).collect())
        .unwrap_or_default()
}
