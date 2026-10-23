//! Per-sample progress (the Lab view's sample × step grid and "9 of 12 samples done"): which sample
//! a task belongs to, and how a run's samples stand.
//!
//! A **sample** is a row of the samplesheet biffix wrote for the run (or, with no sheet, a value a
//! task key names). A **step** is a task group (Snakemake rule / Nextflow process). A task belongs
//! to a sample when its key names that sample; a step is per-sample when at least one of its tasks
//! does (or the author forced it to `sample`), and whole-study otherwise.
//!
//! Everything here is a pure function of task keys, the run's [`SampleSpec`] and its state, so the
//! stored attribution is a function of the event log (a rule fix is a replay) and every reader
//! counts the same way.

mod attribution;
mod grid;
#[cfg(test)]
mod tests;

pub use attribution::{attribute, SampleMatcher};
pub use grid::{
    sample_ids, Cell, CellState, RunEnd, SampleCounts, SampleGrid, SampleRow, SampleState, Step,
    TaskFact,
};

// Named only by the module doc.
#[cfg(doc)]
use crate::event::SampleSpec;
