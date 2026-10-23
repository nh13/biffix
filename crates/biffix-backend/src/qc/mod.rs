//! Reading what a pipeline's QC tools wrote, for the QC viewer: MultiQC's General Statistics
//! ([`multiqc`]), FastQC's modules ([`fastqc`]), where an alignment viewer should open
//! ([`locus`]), a run's reports ([`reports`]), and what the QC viewer shows of one output
//! ([`viewer`]). biffix never runs these tools.

pub(crate) mod fastqc;
pub(crate) mod locus;
pub(crate) mod multiqc;
pub(crate) mod reports;
pub(crate) mod viewer;
