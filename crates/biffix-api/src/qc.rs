//! `qc` — QC previews of a run's outputs: a FastQC result as data, and what a genome browser needs
//! to show an alignment (short-lived links to the file, its index and a reference).

use serde::{Deserialize, Serialize};

/// A FastQC result as data (`GET /runs/{run_id}/outputs/{output_id}/fastqc`): the modules of the
/// `fastqc_data.txt` inside the pipeline's own `_fastqc.zip`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FastqcReport {
    /// The `_fastqc.zip` output the modules were read from.
    pub output_id: String,
    /// The FastQC HTML report beside the zip, when the run registered one (open it with the
    /// output's view URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_output_id: Option<String>,
    /// The FastQC version that wrote it (`0.12.1`), from the file's first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The modules in the file's order.
    pub modules: Vec<FastqcModule>,
}

/// One FastQC module (`>>Per base sequence quality\tpass` … `>>END_MODULE`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FastqcModule {
    /// The module's name as FastQC writes it (`Per base sequence quality`).
    pub name: String,
    /// `pass`, `warn` or `fail` as FastQC wrote it. Open: a later FastQC may write others.
    pub status: String,
    /// The table's column names (its last `#` header line, without the `#`).
    pub columns: Vec<String>,
    /// The table's rows, each cell as written (numbers are not parsed; a cell may be a range such
    /// as `10-14`).
    pub rows: Vec<Vec<String>>,
    /// Header lines before the column names, as `[key, value]` (`["Total Deduplicated
    /// Percentage", "94.08"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<[String; 2]>,
    /// The module held more than the API returns (more rows, or more cells in a row, than its caps,
    /// or the file's data ran past what one response carries); `rows` holds the first ones.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Per-sample FastQC statuses from the run's MultiQC data (`GET /runs/{run_id}/qc/fastqc-status`):
/// for a pipeline that publishes MultiQC's output but not FastQC's own `_fastqc.zip`s, which the
/// FastQC viewer needs. Statuses and basic statistics only: MultiQC's data holds no FastQC plots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FastqcStatusReport {
    /// Where the statuses were read from: `multiqc` (`multiqc_data/multiqc_fastqc.txt`). Open:
    /// other sources may follow.
    pub source: String,
    /// The MultiQC report whose data these are (an output of the run; open it for the plots).
    pub report_output_id: String,
    /// One per FastQC result MultiQC read (a paired-end sample has two), section by section (the
    /// unanchored `multiqc_fastqc.txt` first, then each `multiqc_fastqc_<anchor>.txt` by name);
    /// within a section the run's samples first in the run's sample order, then any result no
    /// sample owns, each group by MultiQC name.
    pub samples: Vec<FastqcStatusSample>,
}

/// One FastQC result as MultiQC summarised it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FastqcStatusSample {
    /// The run's sample this result belongs to: the longest sample id the MultiQC name starts with,
    /// followed by the end or a character that cannot continue an id (`S1_R1` is S1's, `S10_R1`
    /// S10's); when no id owns the MultiQC name (MultiQC renamed, prefixed or truncated it), the
    /// same rule on `filename`'s stem. Ids match literally: one holding `*` or `?` is no glob here.
    /// The rule cannot tell a read suffix from an id: with samples `S1` and `S1_1`, the result
    /// `S1_1` (S1's read 1, as nf-core names reads) is `S1_1`'s. `None` when no sample owns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
    /// The MultiQC FastQC section this result is from, when the report has several (FastQC run on
    /// raw and on trimmed reads): the anchor in its table's name, as written
    /// (`multiqc_fastqc_fastqc_trimmed.txt` → `fastqc_trimmed`; a repeated unanchored section's is
    /// `fastqc-1`). `None` for the unanchored `multiqc_fastqc.txt`. The same MultiQC name can be in
    /// several sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// MultiQC's name for it (usually the FASTQ's name without its extension: `S1_R1`).
    pub fastqc_name: String,
    /// The FASTQ FastQC read (`S1_R1.fastq.gz`), when MultiQC recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// FastQC's modules in MultiQC's column order.
    pub modules: Vec<FastqcModuleStatus>,
    pub stats: FastqcBasicStats,
}

/// One FastQC module's status, without its data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FastqcModuleStatus {
    /// The module's name as FastQC writes it (`Per base sequence quality`).
    pub name: String,
    /// `pass`, `warn` or `fail`, in the case MultiQC wrote it (compare case-insensitively). A cell
    /// that is none of these is no module.
    pub status: String,
}

/// FastQC's basic statistics as MultiQC recorded them (each absent when MultiQC had none).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FastqcBasicStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_sequences: Option<u64>,
    /// As FastQC writes it (`150 kbp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bases: Option<String>,
    /// Reads FastQC flagged as poor quality (FastQC 0.10's `Filtered Sequences`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poor_quality: Option<u64>,
    /// A length or a range (`100`, `35-151`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_length: Option<String>,
    /// GC content, a percent (0–100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_gc: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_sequence_length: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_sequence_length: Option<f64>,
    /// Duplicate reads, a percent (0–100), as MultiQC reports them: 100 − FastQC's total
    /// deduplicated percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_duplicates: Option<f64>,
}

/// `GET /runs/{run_id}/outputs/{output_id}/alignment-view` query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct AlignmentViewQuery {
    /// A reference bundle id to show the alignment against, instead of the run's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Find where to open (`locus`; default true). `false` skips reading the index and header, for
    /// a client only renewing its links: the response then has no `locus`.
    #[serde(default = "default_true")]
    pub locus: bool,
}

impl Default for AlignmentViewQuery {
    fn default() -> Self {
        Self { reference: None, locus: true }
    }
}

fn default_true() -> bool {
    true
}

/// What a genome browser needs to show one alignment output
/// (`GET /runs/{run_id}/outputs/{output_id}/alignment-view`). Every URL is a presigned GET the
/// browser reads directly with HTTP Range requests; they stop working at `expires_at` (or sooner,
/// when the credentials that signed them end), so a client that stays open asks again before
/// then, and when a read is refused, with `?locus=false` (only the links change).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlignmentLinks {
    pub alignment: AlignmentTrack,
    /// The reference to show it against; `None` when the run recorded no reference bundle and none
    /// was asked for (the client offers a choice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<AlignmentReference>,
    /// Where to open: the first contig with mapped reads, as `name:start-end` (1-based, inclusive),
    /// when the file's index and its own header say. `None` ⇒ the browser's own default (and
    /// always with `?locus=false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locus: Option<String>,
    /// Epoch seconds after which the URLs no longer work.
    pub expires_at: f64,
    /// Whether every bucket the URLs point into is known to allow the browser's cross-origin Range
    /// reads: `configured`, or `unknown` (the deploy has not recorded a CORS rule for one of them;
    /// the reads may still work). Open.
    pub bucket_cors: String,
    /// Why something is missing or was left out (an annotation too large to load unindexed, a
    /// reference file outside every data source, …), for the viewer to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The alignment file and its index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlignmentTrack {
    /// The file's name (`S1.markdup.bam`).
    pub name: String,
    /// `bam` or `cram`.
    pub format: String,
    pub url: String,
    /// The index (`.bai`, `.csi` or `.crai`).
    pub index_url: String,
}

/// The reference an alignment is shown against: a bundle's own FASTA (and annotation), or a genome
/// the genome browser hosts (the bundle's `igv_genome`), which sends the browser to that host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AlignmentReference {
    /// The bundle's FASTA, through presigned URLs.
    Bundle {
        bundle_id: String,
        display_name: String,
        fasta_url: String,
        /// The FASTA's `.fai`.
        fai_url: String,
        /// The `.gzi` of a bgzipped FASTA.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gzi_url: Option<String>,
        /// The bundle's gene annotation, when it can be loaded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotation: Option<AnnotationTrack>,
    },
    /// A genome the browser hosts, by its id (`hg38`, `mm39`).
    IgvGenome {
        genome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bundle_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
}

/// A gene annotation track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AnnotationTrack {
    pub url: String,
    /// The tabix index of a bgzipped annotation; `None` ⇒ the whole (small) file is loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_url: Option<String>,
    /// `gtf` or `gff3`.
    pub format: String,
}
