//! Samples from files, and samplesheets from samples.
//!
//! [`detect`](fn@detect) groups sequencing files into samples: each file name yields a sample id
//! and a read (1 or 2, and a lane when Illumina names one), by a rule — the automatic reading of
//! common naming schemes, the file-name parts a person clicked, or a regex. One implementation
//! serves the launch form's live preview (through the backend) and any other caller, so they never
//! disagree.
//! (Merging two detected samples, the fix for a naming slip, happens in the form, on sheet rows.)
//!
//! [`SheetSpec`] is a samplesheet's columns as the workflow declares them. [`SheetSpec::validate`]
//! checks rows against it and [`SheetSpec::render`] writes them as the CSV/TSV the engine reads.

mod detect;
mod names;
mod sheet;
#[cfg(test)]
mod tests;

pub use detect::{
    detect, DetectFile, DetectRule, DetectedSample, Detection, IdFrom, IdMethod, Lanes,
    MergeSuggestion, SampleDetectError,
};
pub use names::{stem, tokenize, Token};
pub use sheet::{
    detect_for_sheet, ColumnRole, SheetColumn, SheetFormat, SheetParseError, SheetProblem,
    SheetRow, SheetSpec,
};
pub(crate) use sheet::{role_from_name, spec_from_row_schema_text};
