//! Shared `JsonSchema` for the contract's **open enums**.
//!
//! On the wire an open enum is *always a string*: the known values are a documented subset, but
//! any string is valid (that is what "open, unknown-tolerant" means). The derived schemars schema
//! wrongly modelled the `#[serde(untagged)] Unknown(String)` variant as an object, so it rejected
//! the bare-string unknowns the code actually emits. This helper emits the correct schema: a
//! string, with the known values surfaced as non-exclusive `examples` for documentation.

use schemars::{json_schema, Schema};

/// Build a "string, open enum" schema whose known values are documentation-only examples.
pub(crate) fn open_string_enum_schema(known: &[&str], description: &str) -> Schema {
    let mut schema = json_schema!({
        "type": "string",
        "description": description,
    });
    if !known.is_empty() {
        schema.insert("examples".into(), known.into());
    }
    schema
}

/// Macro: implement `JsonSchema` for an open-enum type as an open string schema.
///
/// Crate-internal: it is used only by the enum definitions in this crate, so it is a plain
/// `macro_rules!` (no `#[macro_export]`, which would hoist it and `open_string_enum_schema` into
/// biffix-core's public API) re-exported `pub(crate)` for the `crate::impl_open_enum_schema!`
/// call sites.
macro_rules! impl_open_enum_schema {
    ($ty:ty, $name:literal, $desc:literal, [$($known:literal),* $(,)?]) => {
        impl schemars::JsonSchema for $ty {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                $name.into()
            }
            fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
                $crate::open_enum_schema::open_string_enum_schema(&[$($known),*], $desc)
            }
            fn inline_schema() -> bool {
                true
            }
        }
    };
}
pub(crate) use impl_open_enum_schema;
