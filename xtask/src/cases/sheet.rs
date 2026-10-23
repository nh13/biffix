//! A table of samplesheet cases with their expected results, shared across languages.
//!
//! The UI checks, renders and counts samplesheet rows live with a TypeScript port of
//! [`SheetSpec::validate`], [`SheetSpec::render`] and [`SheetSpec::sample_count`]; the server stays
//! authoritative. To keep the two from drifting, these cases are asserted against the Rust code
//! (`tests` below) and emitted to `schemas/sheet-cases.json` (by `cargo xtask emit-schemas`), which
//! the UI's tests replay against its port.

use serde::Serialize;

use biffix_workflow::samples::{
    ColumnRole, SheetColumn, SheetFormat, SheetProblem, SheetRow, SheetSpec,
};

#[derive(Debug, Serialize)]
pub(crate) struct SheetCase {
    name: &'static str,
    spec: SheetSpec,
    rows: Vec<SheetRow>,
    /// What `validate` reports, in order.
    problems: Vec<SheetProblem>,
    /// What `render` writes.
    rendered: String,
    /// What `sample_count` counts.
    sample_count: u32,
}

fn col(name: &str) -> SheetColumn {
    SheetColumn { name: name.into(), ..SheetColumn::default() }
}

fn fastq(name: &str, role: ColumnRole, required: bool) -> SheetColumn {
    SheetColumn {
        role: Some(role),
        required,
        file: true,
        suffixes: vec![".fastq.gz".into()],
        ..col(name)
    }
}

/// An nf-core/rnaseq-like sheet: an id, two FASTQ columns and a choice with a default.
fn rnaseq() -> SheetSpec {
    SheetSpec {
        columns: vec![
            SheetColumn { role: Some(ColumnRole::SampleId), required: true, ..col("sample") },
            fastq("fastq_1", ColumnRole::Read1, true),
            fastq("fastq_2", ColumnRole::Read2, false),
            SheetColumn {
                required: true,
                enum_values: Some(vec!["auto".into(), "forward".into()]),
                default: Some("auto".into()),
                ..col("strandedness")
            },
        ],
        ..SheetSpec::default()
    }
}

/// An id and a free-text column, for quoting.
fn notes(format: SheetFormat) -> SheetSpec {
    SheetSpec {
        columns: vec![
            SheetColumn { role: Some(ColumnRole::SampleId), ..col("sample") },
            col("note"),
        ],
        format,
        ..SheetSpec::default()
    }
}

fn row(cells: &[(&str, &str)]) -> SheetRow {
    cells.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn at(row: usize, column: &str, message: &str) -> SheetProblem {
    SheetProblem { row: Some(row), column: Some(column.into()), message: message.into() }
}

/// The cases. Each exercises one rule of the checks, the writer or the count.
pub(crate) fn cases() -> Vec<SheetCase> {
    let x = "s3://b/x.fastq.gz";
    vec![
        SheetCase {
            name: "a valid sheet, single- and paired-end",
            spec: rnaseq(),
            rows: vec![
                row(&[
                    ("sample", "A"),
                    ("fastq_1", "s3://b/A_R1.fastq.gz"),
                    ("fastq_2", "s3://b/A_R2.fastq.gz"),
                    ("strandedness", "auto"),
                ]),
                row(&[
                    ("sample", "B"),
                    ("fastq_1", "s3://b/B_R1.fastq.gz"),
                    ("strandedness", "forward"),
                ]),
            ],
            problems: vec![],
            rendered: "sample,fastq_1,fastq_2,strandedness\n\
                       A,s3://b/A_R1.fastq.gz,s3://b/A_R2.fastq.gz,auto\n\
                       B,s3://b/B_R1.fastq.gz,,forward\n"
                .into(),
            sample_count: 2,
        },
        SheetCase {
            name: "no rows",
            spec: rnaseq(),
            rows: vec![],
            problems: vec![SheetProblem { row: None, column: None, message: "no samples".into() }],
            rendered: "sample,fastq_1,fastq_2,strandedness\n".into(),
            sample_count: 0,
        },
        SheetCase {
            name: "each cell rule, unknown columns first",
            spec: SheetSpec { paired_only: true, ..rnaseq() },
            rows: vec![
                row(&[
                    ("sample", " "),
                    ("fastq_1", "local/a.fastq.gz"),
                    ("strandedness", "sideways"),
                ]),
                row(&[
                    ("batch", "7"),
                    ("sample", "B"),
                    ("fastq_1", "s3://b/B.txt"),
                    ("fastq_2", "s3://b/B_R2.FASTQ.GZ"),
                    ("strandedness", "forward"),
                ]),
            ],
            problems: vec![
                at(0, "sample", "required"),
                at(0, "fastq_1", "must be an s3:// path"),
                at(0, "strandedness", "not one of auto, forward"),
                at(0, "fastq_2", "this workflow needs paired-end reads"),
                at(1, "batch", "not a column of this samplesheet"),
                at(1, "fastq_1", "must end in .fastq.gz"),
            ],
            rendered: "sample,fastq_1,fastq_2,strandedness\n\
                       ,local/a.fastq.gz,,sideways\n\
                       B,s3://b/B.txt,s3://b/B_R2.FASTQ.GZ,forward\n"
                .into(),
            sample_count: 1,
        },
        SheetCase {
            name: "each repeat of a file names the row before it",
            spec: rnaseq(),
            rows: ["A", "B", "C"]
                .iter()
                .map(|id| row(&[("sample", id), ("fastq_1", x), ("strandedness", "auto")]))
                .collect(),
            problems: vec![
                at(1, "fastq_1", "the same file as row 1"),
                at(2, "fastq_1", "the same file as row 2"),
            ],
            rendered: format!(
                "sample,fastq_1,fastq_2,strandedness\nA,{x},,auto\nB,{x},,auto\nC,{x},,auto\n"
            ),
            sample_count: 3,
        },
        SheetCase {
            name: "one file as both reads of a row",
            spec: rnaseq(),
            rows: vec![row(&[
                ("sample", "A"),
                ("fastq_1", x),
                ("fastq_2", x),
                ("strandedness", "auto"),
            ])],
            problems: vec![at(0, "fastq_2", "the same file as fastq_1")],
            rendered: format!("sample,fastq_1,fastq_2,strandedness\nA,{x},{x},auto\n"),
            sample_count: 1,
        },
        SheetCase {
            name: "a shared file repeats freely; a read file used as read 1 and read 2 does not",
            spec: SheetSpec {
                columns: vec![
                    SheetColumn { role: Some(ColumnRole::SampleId), ..col("sample") },
                    fastq("fastq_1", ColumnRole::Read1, true),
                    fastq("fastq_2", ColumnRole::Read2, false),
                    SheetColumn { file: true, ..col("panel") },
                ],
                ..SheetSpec::default()
            },
            rows: vec![
                row(&[("sample", "A"), ("fastq_1", x), ("panel", "s3://b/p.bed")]),
                row(&[
                    ("sample", "B"),
                    ("fastq_1", "s3://b/y.fastq.gz"),
                    ("fastq_2", x),
                    ("panel", "s3://b/p.bed"),
                ]),
            ],
            problems: vec![at(1, "fastq_2", "the same file as row 1")],
            rendered: format!(
                "sample,fastq_1,fastq_2,panel\nA,{x},,s3://b/p.bed\n\
                 B,s3://b/y.fastq.gz,{x},s3://b/p.bed\n"
            ),
            sample_count: 2,
        },
        SheetCase {
            name: "ids count trimmed, as they are written, and empty rows are left out",
            spec: notes(SheetFormat::Csv),
            rows: vec![
                row(&[("sample", "A")]),
                row(&[("sample", "A ")]),
                row(&[("sample", " ")]),
                row(&[("sample", " "), ("note", "no id")]),
                row(&[("sample", "B")]),
            ],
            problems: vec![],
            rendered: "sample,note\nA,\nA,\n,no id\nB,\n".into(),
            sample_count: 2,
        },
        SheetCase {
            name: "a sheet of only empty rows has no samples",
            spec: rnaseq(),
            rows: vec![
                row(&[("sample", ""), ("fastq_1", " "), ("fastq_2", ""), ("strandedness", "")]),
                row(&[]),
            ],
            problems: vec![SheetProblem { row: None, column: None, message: "no samples".into() }],
            rendered: "sample,fastq_1,fastq_2,strandedness\n".into(),
            sample_count: 0,
        },
        SheetCase {
            name: "an empty row among samples is left out, with no problems of its own",
            spec: rnaseq(),
            rows: vec![
                row(&[
                    ("sample", "A"),
                    ("fastq_1", "s3://b/A_R1.fastq.gz"),
                    ("strandedness", "auto"),
                ]),
                row(&[("sample", " "), ("fastq_1", ""), ("strandedness", "")]),
                row(&[("sample", "B"), ("strandedness", "auto")]),
            ],
            problems: vec![at(2, "fastq_1", "required")],
            rendered: "sample,fastq_1,fastq_2,strandedness\n\
                       A,s3://b/A_R1.fastq.gz,,auto\n\
                       B,,,auto\n"
                .into(),
            sample_count: 2,
        },
        SheetCase {
            name: "CSV quotes a cell with a comma, quote or newline, and trims cells",
            spec: notes(SheetFormat::Csv),
            rows: vec![
                row(&[("sample", "a,b"), ("note", "say \"hi\"")]),
                row(&[("sample", "c"), ("note", "two\nlines")]),
                row(&[("sample", "  d  "), ("note", "plain")]),
            ],
            problems: vec![],
            rendered: "sample,note\n\"a,b\",\"say \"\"hi\"\"\"\nc,\"two\nlines\"\nd,plain\n".into(),
            sample_count: 3,
        },
        SheetCase {
            name: "TSV quotes a cell with a tab, quote or newline, as Python's csv writer does",
            spec: notes(SheetFormat::Tsv),
            rows: vec![
                row(&[("sample", "a,b"), ("note", "x\ty\nz")]),
                row(&[("sample", "q"), ("note", "\"q\"")]),
            ],
            problems: vec![],
            rendered: "sample\tnote\na,b\t\"x\ty\nz\"\nq\t\"\"\"q\"\"\"\n".into(),
            sample_count: 2,
        },
        SheetCase {
            name: "without a sample-id column every row that is not empty is a sample",
            spec: SheetSpec {
                columns: vec![SheetColumn { file: true, ..col("file") }],
                ..SheetSpec::default()
            },
            rows: vec![row(&[("file", "s3://b/1")]), row(&[("file", "s3://b/2")]), row(&[])],
            problems: vec![],
            rendered: "file\ns3://b/1\ns3://b/2\n".into(),
            sample_count: 2,
        },
    ]
}

/// The cases as the JSON the UI replays.
pub(crate) fn cases_json() -> String {
    crate::artifacts::pretty_json(&cases())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_holds_for_the_rust_sheet() {
        for case in cases() {
            assert_eq!(case.spec.validate(&case.rows), case.problems, "validate: {}", case.name);
            assert_eq!(case.spec.render(&case.rows), case.rendered, "render: {}", case.name);
            assert_eq!(
                case.spec.sample_count(&case.rows),
                case.sample_count,
                "sample_count: {}",
                case.name
            );
        }
    }
}
