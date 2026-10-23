//! The RunEvent `source` open enum.
//!
//! Open by contract: a future adapter adds a value without a contract bump. Unknown values
//! are preserved (stored + audited), never dropped.
//!
//! The normalized workflow **source tree** the version id is computed over lives in the workflow
//! `version` module, next to the content hash it feeds.

use serde::{Deserialize, Serialize};

/// Declares [`EventSource`] from ONE `Variant => "wire"` table: each variant's serde name,
/// [`EventSource::as_str`], [`EventSource::KNOWN`] and the schema's documented values all expand
/// from it, so a new source cannot reach one of them without reaching the others.
macro_rules! sources {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        /// Who produced the event.
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum EventSource {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
            /// Any value not known to this build. The raw string is preserved.
            #[serde(untagged)]
            Unknown(String),
        }

        impl EventSource {
            /// Every source this build knows (all but [`EventSource::Unknown`]), in declaration
            /// order.
            pub const KNOWN: &'static [EventSource] = &[$(EventSource::$variant),+];

            /// The canonical wire string for this source (its serde value). Shared so SQL that
            /// filters on `source` (e.g. reconciliation keying on `aws-batch`) cannot drift from
            /// the enum.
            pub fn as_str(&self) -> &str {
                match self {
                    $(EventSource::$variant => $wire,)+
                    EventSource::Unknown(raw) => raw,
                }
            }
        }

        // The schema keeps the name `Source` (the published `$defs` key) whatever the Rust name.
        crate::impl_open_enum_schema!(
            EventSource,
            "Source",
            "Who produced the event (open enum).",
            [$($wire),+]
        );
    };
}

sources! {
    AwsBatch => "aws-batch",
    AwsEc2 => "aws-ec2",
    AwsCodebuild => "aws-codebuild",
    Nextflow => "nextflow",
    Snakemake => "snakemake",
    BiffixControlPlane => "biffix-control-plane",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_as_str_matches_serde() {
        // `as_str` and the serde name expand from the same table; pin that they agree (and
        // round-trip) for every known variant, so SQL built from `as_str` matches what is stored.
        for source in EventSource::KNOWN {
            let wire = serde_json::to_value(source).unwrap();
            assert_eq!(wire.as_str().unwrap(), source.as_str());
            assert_eq!(&serde_json::from_value::<EventSource>(wire).unwrap(), source);
        }
    }

    #[test]
    fn unknown_source_round_trips_raw() {
        let raw = serde_json::json!("aws-lambda");
        let source: EventSource = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(source, EventSource::Unknown("aws-lambda".into()));
        assert_eq!(source.as_str(), "aws-lambda");
        assert_eq!(serde_json::to_value(&source).unwrap(), raw);
    }

    #[test]
    fn source_schema_examples_match_variants() {
        // The schema's documented values must be exactly the known variants' wire strings.
        let variant_tags: Vec<String> =
            EventSource::KNOWN.iter().map(|s| s.as_str().to_string()).collect();

        let mut generator = schemars::SchemaGenerator::default();
        let schema = <EventSource as schemars::JsonSchema>::json_schema(&mut generator);
        assert_eq!(
            schema.get("type"),
            Some(&serde_json::json!("string")),
            "an open-enum schema must be a plain string schema"
        );
        let examples: Vec<String> = schema
            .get("examples")
            .and_then(serde_json::Value::as_array)
            .expect("open-enum schema carries examples")
            .iter()
            .map(|v| v.as_str().expect("example is a string").to_string())
            .collect();
        assert_eq!(examples, variant_tags);
    }
}
