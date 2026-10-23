//! `samples` — sample detection from file names (`POST /samples/detect`). The detection itself is
//! `biffix_workflow::samples`'; these are its request and response.
//!
//! Their parts are biffix-workflow types with only a schemars schema, so the OpenAPI document
//! publishes the body as an untyped object and the UI's types come from
//! `schemas/samples.schema.json` ([`SamplesContract`]) instead.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use biffix_workflow::samples::{DetectFile, DetectRule, Detection, SheetRow, SheetSpec, Token};

/// Files to detect samples in, the rule, and (optionally) the samplesheet to fill.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SampleDetectRequest {
    pub files: Vec<DetectFile>,
    #[serde(default)]
    pub rule: DetectRule,
    /// When given, the response includes the samplesheet rows for the detected samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samplesheet: Option<SheetSpec>,
}

/// What detection found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DetectedSamples {
    pub detection: Detection,
    /// Rows for the samplesheet in the request (empty without one).
    pub rows: Vec<SheetRow>,
    /// The first file's name split into parts, for choosing the parts that make up the id.
    pub tokens: Vec<Token>,
    /// The first file's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

/// The samples contract as one JSON Schema document (request and response).
#[derive(JsonSchema)]
pub struct SamplesContract {
    pub request: SampleDetectRequest,
    pub response: DetectedSamples,
}
