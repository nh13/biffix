use super::*;
use crate::samples::detect::{auto_read, folder_name};
use crate::samples::names::{ends_with_ignore_case, loose, natural_cmp, tokenize};
use crate::samples::sheet::{endings_from_pattern, spec_from_row_schema};
use crate::text::basename;

fn files(names: &[&str]) -> Vec<DetectFile> {
    names
        .iter()
        .map(|n| DetectFile { uri: format!("s3://b/2026/batch-12/{n}"), size: None })
        .collect()
}

fn ids(d: &Detection) -> Vec<&str> {
    d.samples.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn illumina_names_pair_and_merge_lanes() {
    let d = detect(
        &files(&[
            "P01_S1_L001_R1_001.fastq.gz",
            "P01_S1_L001_R2_001.fastq.gz",
            "P01_S1_L002_R1_001.fastq.gz",
            "P01_S1_L002_R2_001.fastq.gz",
            "P02_S2_L001_R1_001.fastq.gz",
            "P02_S2_L001_R2_001.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["P01", "P02"]);
    assert_eq!(d.samples[0].lanes, 2);
    assert_eq!(d.samples[0].read1.len(), 2);
    assert!(d.samples[0].problems.is_empty());
}

#[test]
fn simple_suffix_schemes_pair() {
    let d = detect(
        &files(&["a_R1.fq.gz", "a_R2.fq.gz", "b_1.fastq.gz", "b_2.fastq.gz", "c.fastq.gz"]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["a", "b", "c"]);
    assert_eq!(d.samples[1].read2.len(), 1);
    assert!(d.samples[2].read2.is_empty() && d.samples[2].problems.is_empty());
}

#[test]
fn a_naming_slip_is_offered_as_a_merge() {
    let d = detect(
        &files(&["P7_S7_L001_R1_001.fastq.gz", "P07_S7_L001_R2_001.fastq.gz"]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["P07", "P7"]);
    assert_eq!(d.samples[0].problems, ["read 2 without read 1"]);
    assert_eq!(
        d.suggestions,
        [MergeSuggestion { ids: ["P7".into(), "P07".into()], into: "P07".into() }]
    );
}

#[test]
fn lanes_can_be_separate_samples_or_a_problem() {
    let names = ["P01_S1_L001_R1_001.fastq.gz", "P01_S1_L002_R1_001.fastq.gz"];
    let separate = DetectRule { lanes: Some(Lanes::Separate), ..DetectRule::default() };
    assert_eq!(ids(&detect(&files(&names), &separate).unwrap()), ["P01_L001", "P01_L002"]);
    let error = DetectRule { lanes: Some(Lanes::Error), ..DetectRule::default() };
    let d = detect(&files(&names), &error).unwrap();
    assert_eq!(d.samples[0].problems[0], "2 lanes; this workflow takes one file per read");
}

#[test]
fn chosen_parts_make_the_id() {
    let name = "PRJ9_P07_S7_L001_R1_001.fastq.gz";
    let tokens = tokenize(name);
    assert_eq!(tokens[2].text, "P07");
    assert!(tokens[1].separator);
    let rule =
        DetectRule { method: IdMethod::Tokens { selected: vec![2] }, ..DetectRule::default() };
    let d =
        detect(&files(&[name, "PRJ9_P07_S7_L001_R2_001.fastq.gz", "x.fastq.gz"]), &rule).unwrap();
    assert_eq!(ids(&d), ["P07"]);
    assert_eq!(d.samples[0].read2.len(), 1);
    // A name too short for the chosen part is left unmatched, not mangled.
    assert_eq!(d.unmatched, ["s3://b/2026/batch-12/x.fastq.gz"]);
}

#[test]
fn a_regex_group_makes_the_id() {
    let rule = DetectRule {
        method: IdMethod::Regex { pattern: r"^PRJ9_(?P<sample>P\d+)_".into() },
        ..DetectRule::default()
    };
    let d = detect(&files(&["PRJ9_P07_R1.fq.gz", "other.fq.gz"]), &rule).unwrap();
    assert_eq!(ids(&d), ["P07"]);
    assert_eq!(d.unmatched.len(), 1);
    let bad =
        DetectRule { method: IdMethod::Regex { pattern: "(".into() }, ..DetectRule::default() };
    // The parser's reason on one line, not its report with carets under the pattern.
    match detect(&[], &bad) {
        Err(SampleDetectError::BadRegex(reason)) => {
            assert!(!reason.contains('\n') && !reason.is_empty(), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    let nogroup =
        DetectRule { method: IdMethod::Regex { pattern: "P".into() }, ..DetectRule::default() };
    assert_eq!(detect(&[], &nogroup), Err(SampleDetectError::NoGroup));
}

#[test]
fn the_parent_folder_can_prefix_the_id_and_gz_wins() {
    let rule = DetectRule { id_from: IdFrom::NameAndParent, ..DetectRule::default() };
    let d = detect(&files(&["a_R1.fastq", "a_R1.fastq.gz"]), &rule).unwrap();
    assert_eq!(ids(&d), ["batch-12_a"]);
    assert_eq!(d.skipped, ["s3://b/2026/batch-12/a_R1.fastq"]);
}

#[test]
fn files_named_only_by_read_take_the_folder_name() {
    let d = detect(&files(&["r1.fq.gz", "r2.fq.gz"]), &DetectRule::default()).unwrap();
    assert_eq!(ids(&d), ["batch-12"]);
    assert_eq!(d.samples[0].read1.len(), 1);
    assert_eq!(d.samples[0].read2.len(), 1);
    assert!(d.samples[0].problems.is_empty());

    let d = detect(
        &files(&["A/R1.FASTQ.GZ", "A/R2.FASTQ.GZ", "B/read_1.fq", "B/2.fq"]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["A", "B"]);
    assert!(d.samples.iter().all(|s| s.read1.len() == 1 && s.read2.len() == 1));

    // The folder is not doubled when the rule also asks for it.
    let rule = DetectRule { id_from: IdFrom::NameAndParent, ..DetectRule::default() };
    let d = detect(&files(&["r1.fq.gz", "r2.fq.gz"]), &rule).unwrap();
    assert_eq!(ids(&d), ["batch-12"]);
}

#[test]
fn a_read_only_name_at_the_bucket_root_keeps_its_stem() {
    let root = |n: &str| DetectFile { uri: format!("s3://b/{n}"), size: None };
    let d = detect(&[root("r1.fq.gz")], &DetectRule::default()).unwrap();
    assert_eq!(ids(&d), ["r1"]);
    assert!(d.samples[0].problems.is_empty());
    // Names that merely end in a read number are unchanged.
    let d = detect(&files(&["tr1.fq.gz", "xr_1.fq.gz"]), &DetectRule::default()).unwrap();
    assert_eq!(ids(&d), ["tr1", "xr"]);
}

/// The lanes the form's merge reads off names (`fileLane` in
/// `ui/src/lib/launch/samplesheet/sheet.ts`, whose test lists the same names) are the lanes
/// detection and `rows_from` read.
#[test]
fn lanes_read_off_names_as_the_form_reads_them() {
    let lane = |uri: &str| auto_read(stem(basename(uri))).lane;
    assert_eq!(lane("s3://b/P01_S1_L001_R1_001.fastq.gz").as_deref(), Some("001"));
    assert_eq!(lane("s3://b/P01_L002_R2.FQ.GZ").as_deref(), Some("002"));
    assert_eq!(lane("s3://b/P01_L002.R2.fq").as_deref(), Some("002"));
    assert_eq!(lane("s3://b/P01_S1_R1_001.fastq.gz"), None);
    assert_eq!(lane("s3://b/P01_L001_1.fq.gz"), None);
    assert_eq!(lane("s3://b/P01_L001_S1_R1.fq.gz"), None);
}

#[test]
fn natural_order_counts_numbers() {
    let mut v = vec!["P10", "P2", "P1", "p3"];
    v.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(v, ["P1", "P2", "p3", "P10"]);
    assert_eq!(loose("P007"), loose("p7"));
    assert_ne!(loose("P70"), loose("P7"));
}

fn rnaseq_spec() -> SheetSpec {
    SheetSpec {
        columns: vec![
            SheetColumn {
                name: "sample".into(),
                role: Some(ColumnRole::SampleId),
                required: true,
                ..Default::default()
            },
            SheetColumn {
                name: "fastq_1".into(),
                role: Some(ColumnRole::Read1),
                required: true,
                file: true,
                suffixes: vec![".fastq.gz".into()],
                ..Default::default()
            },
            SheetColumn {
                name: "fastq_2".into(),
                role: Some(ColumnRole::Read2),
                file: true,
                ..Default::default()
            },
            SheetColumn {
                name: "strandedness".into(),
                enum_values: Some(vec!["auto".into(), "forward".into()]),
                default: Some("auto".into()),
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

#[test]
fn detected_samples_become_rows_one_per_lane_pair() {
    let d = detect(
        &files(&[
            "P01_S1_L001_R1_001.fastq.gz",
            "P01_S1_L001_R2_001.fastq.gz",
            "P01_S1_L002_R1_001.fastq.gz",
            "P01_S1_L002_R2_001.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    let rows = rnaseq_spec().rows_from(&d.samples);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["sample"], "P01");
    assert_eq!(
        pairs(&rows),
        [
            ("P01_S1_L001_R1_001", "P01_S1_L001_R2_001"),
            ("P01_S1_L002_R1_001", "P01_S1_L002_R2_001"),
        ]
    );
    assert_eq!(rows[0]["strandedness"], "auto");
    assert!(rnaseq_spec().validate(&rows).is_empty());
}

/// Each row's (read 1, read 2) file stems.
fn pairs(rows: &[SheetRow]) -> Vec<(&str, &str)> {
    rows.iter().map(|r| (stem(basename(&r["fastq_1"])), stem(basename(&r["fastq_2"])))).collect()
}

#[test]
fn mates_pair_within_a_lane_and_an_unbalanced_lane_is_a_problem() {
    // Lane 001 has only read 1 and lane 002 only read 2: the totals balance, the lanes don't.
    let d = detect(
        &files(&["A_S1_L001_R1_001.fastq.gz", "A_S1_L002_R2_001.fastq.gz"]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(
        d.samples[0].problems,
        [
            "lane 001: 1 read-1 files but 0 read-2 files",
            "lane 002: 0 read-1 files but 1 read-2 files",
        ]
    );
    let rows = rnaseq_spec().rows_from(&d.samples);
    // No row joins mates from two lanes; the gap is an empty (required) cell.
    assert_eq!(pairs(&rows), [("A_S1_L001_R1_001", ""), ("", "A_S1_L002_R2_001")]);
    assert_eq!(rnaseq_spec().validate(&rows)[0].message, "required");

    // Three lanes, one short of read 2: the rest still pair lane by lane.
    let d = detect(
        &files(&[
            "P1_S1_L001_R1_001.fastq.gz",
            "P1_S1_L001_R2_001.fastq.gz",
            "P1_S1_L002_R1_001.fastq.gz",
            "P1_S1_L003_R1_001.fastq.gz",
            "P1_S1_L003_R2_001.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(d.samples[0].problems, ["lane 002: 1 read-1 files but 0 read-2 files"]);
    assert_eq!(
        pairs(&rnaseq_spec().rows_from(&d.samples)),
        [
            ("P1_S1_L001_R1_001", "P1_S1_L001_R2_001"),
            ("P1_S1_L002_R1_001", ""),
            ("P1_S1_L003_R1_001", "P1_S1_L003_R2_001"),
        ]
    );
}

#[test]
fn several_read_1_files_with_no_lane_are_a_problem_under_every_lanes_rule() {
    // The same name in two folders, with no lane to tell them apart.
    let names =
        ["run1/A_R1.fastq.gz", "run1/A_R2.fastq.gz", "run2/A_R1.fastq.gz", "run2/A_R2.fastq.gz"];
    for lanes in [Lanes::Merge, Lanes::Separate, Lanes::Error] {
        let rule = DetectRule { lanes: Some(lanes), ..DetectRule::default() };
        let d = detect(&files(&names), &rule).unwrap();
        assert_eq!(d.samples[0].problems, ["2 read-1 files for one sample"], "{lanes:?}");
    }
    // Split parts of one lane (`_001`, `_002`) are ordinary, whether the sample has one lane or
    // several.
    let d = detect(
        &files(&[
            "A_S1_L001_R1_001.fastq.gz",
            "A_S1_L001_R1_002.fastq.gz",
            "A_S1_L001_R2_001.fastq.gz",
            "A_S1_L001_R2_002.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    assert!(d.samples[0].problems.is_empty(), "{:?}", d.samples[0].problems);
    let d = detect(
        &files(&[
            "A_S1_L001_R1_001.fastq.gz",
            "A_S1_L001_R1_002.fastq.gz",
            "A_S1_L001_R2_001.fastq.gz",
            "A_S1_L001_R2_002.fastq.gz",
            "A_S1_L002_R1_001.fastq.gz",
            "A_S1_L002_R2_001.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    assert!(d.samples[0].problems.is_empty());
    assert_eq!(
        pairs(&rnaseq_spec().rows_from(&d.samples)),
        [
            ("A_S1_L001_R1_001", "A_S1_L001_R2_001"),
            ("A_S1_L001_R1_002", "A_S1_L001_R2_002"),
            ("A_S1_L002_R1_001", "A_S1_L002_R2_001"),
        ]
    );
}

#[test]
fn one_name_in_two_folders_pairs_its_mates_by_folder_and_is_a_problem_per_lane() {
    // One library sequenced on two runs: each lane has the same names in `run1/` and `run2/`,
    // and the request lists read 1 and read 2 in different folder orders.
    let d = detect(
        &files(&[
            "run2/A_S1_L001_R1_001.fastq.gz",
            "run1/A_S1_L001_R1_001.fastq.gz",
            "run1/A_S1_L001_R2_001.fastq.gz",
            "run2/A_S1_L001_R2_001.fastq.gz",
            "run1/A_S1_L002_R1_001.fastq.gz",
            "run2/A_S1_L002_R2_001.fastq.gz",
            "run2/A_S1_L002_R1_001.fastq.gz",
            "run1/A_S1_L002_R2_001.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    let rows = rnaseq_spec().rows_from(&d.samples);
    let folder = |uri: &str| uri.rsplit('/').nth(1).unwrap_or("").to_string();
    let mates: Vec<(String, String)> =
        rows.iter().map(|r| (folder(&r["fastq_1"]), folder(&r["fastq_2"]))).collect();
    assert_eq!(
        mates,
        [
            ("run1".to_string(), "run1".to_string()),
            ("run2".to_string(), "run2".to_string()),
            ("run1".to_string(), "run1".to_string()),
            ("run2".to_string(), "run2".to_string()),
        ]
    );
    assert_eq!(
        d.samples[0].problems,
        [
            "lane 001: 2 read-1 files named A_S1_L001_R1_001.fastq.gz",
            "lane 002: 2 read-1 files named A_S1_L002_R1_001.fastq.gz",
        ]
    );
}

#[test]
fn a_lane_split_id_that_names_another_sample_is_renamed_not_merged() {
    let rule = DetectRule { lanes: Some(Lanes::Separate), ..DetectRule::default() };
    // `A` on lane 001 would be `A_L001`, which `A_L001_1.fq.gz` (sample `A_L001`) already is.
    let d = detect(&files(&["A_R1.fq.gz", "A_L001_R1.fq.gz", "A_L001_1.fq.gz"]), &rule).unwrap();
    assert_eq!(ids(&d), ["A", "A_L001", "A_L001_2"]);
    let renamed = &d.samples[2];
    assert_eq!(renamed.read1, ["s3://b/2026/batch-12/A_L001_R1.fq.gz"]);
    assert_eq!(renamed.problems, ["named A_L001_2: another sample is already named A_L001"]);
    assert_eq!(d.samples[1].read1, ["s3://b/2026/batch-12/A_L001_1.fq.gz"]);
}

#[test]
fn index_reads_repeats_and_undetermined_reads_are_set_apart() {
    let d = detect(
        &files(&[
            "P01_S1_L001_R1_001.fastq.gz",
            "P01_S1_L001_R2_001.fastq.gz",
            "P01_S1_L001_I1_001.fastq.gz",
            "P01_S1_L001_I2_001.fastq.gz",
            "P01_S1_L001_R1_001.fastq.gz",
            "Undetermined_S0_L001_R1_001.fastq.gz",
        ]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["P01", "Undetermined"]);
    assert!(d.samples[0].problems.is_empty());
    assert_eq!(
        d.samples[1].problems,
        ["undetermined reads: demultiplexing matched them to no sample"]
    );
    let at = |n: &str| format!("s3://b/2026/batch-12/{n}");
    assert_eq!(
        d.skipped,
        [
            at("P01_S1_L001_I1_001.fastq.gz"),
            at("P01_S1_L001_I2_001.fastq.gz"),
            at("P01_S1_L001_R1_001.fastq.gz"),
        ]
    );
}

#[test]
fn bare_numbers_are_reads_only_in_a_folder_numbered_1_and_2() {
    // Numbered single-end samples are samples, not one pair named after the folder.
    let d = detect(
        &files(&["run42/1.fastq.gz", "run42/2.fastq.gz", "run42/3.fastq.gz"]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["1", "2", "3"]);
    assert!(d.samples.iter().all(|s| s.read2.is_empty()));
    // A folder holding only 1 and 2 is still one pair, and a prefixed marker always is.
    let d = detect(
        &files(&["A/1.fq.gz", "A/2.fq.gz", "B/r1.fq.gz", "B/r2.fq.gz", "B/3.fq.gz"]),
        &DetectRule::default(),
    )
    .unwrap();
    assert_eq!(ids(&d), ["3", "A", "B"]);
    assert_eq!((d.samples[2].read1.len(), d.samples[2].read2.len()), (1, 1));
}

#[test]
fn a_file_at_the_bucket_root_takes_no_parent_prefix() {
    let rule = DetectRule { id_from: IdFrom::NameAndParent, ..DetectRule::default() };
    let root = |n: &str| DetectFile { uri: format!("s3://my-bucket/{n}"), size: None };
    let d = detect(&[root("a_R1.fastq.gz"), root("a_R2.fastq.gz")], &rule).unwrap();
    assert_eq!(ids(&d), ["a"]);
    assert_eq!(folder_name("s3://my-bucket/a.fq"), None);
    assert_eq!(folder_name("s3://my-bucket/x/a.fq"), Some("x"));
    assert_eq!(folder_name("x/a.fq"), Some("x"));
    assert_eq!(folder_name("a.fq"), None);
}

#[test]
fn suffixes_ignore_ascii_case_without_slicing_a_character() {
    assert!(ends_with_ignore_case("s3://b/A.FASTQ", ".fastq"));
    assert!(!ends_with_ignore_case("éfastq", ".fastq"));
    assert!(!ends_with_ignore_case("q", ".fastq"));
    assert_eq!(stem("é.FQ.GZ"), "é");
}

#[test]
fn the_caller_s_lanes_choice_wins_over_the_sheet_s() {
    let files = files(&["P01_S1_L001_R1_001.fastq.gz", "P01_S1_L002_R1_001.fastq.gz"]);
    let sheet = SheetSpec { lanes: Lanes::Separate, ..rnaseq_spec() };
    let rule = |lanes: Option<Lanes>| DetectRule { lanes, ..DetectRule::default() };
    let sample_ids = |lanes| {
        detect_for_sheet(&files, &rule(lanes), Some(&sheet))
            .unwrap()
            .0
            .samples
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(sample_ids(None), ["P01_L001", "P01_L002"]);
    assert_eq!(sample_ids(Some(Lanes::Merge)), ["P01"]);
    // Without a sheet, lanes merge.
    let (no_sheet, rows) = detect_for_sheet(&files, &rule(None), None).unwrap();
    assert_eq!(no_sheet.samples.len(), 1);
    assert!(rows.is_empty());
    // An omitted choice stays omitted on the wire.
    let json = serde_json::to_value(DetectRule::default()).unwrap();
    assert!(json.get("lanes").is_none());
    let explicit: DetectRule = serde_json::from_str(r#"{"lanes":"merge"}"#).unwrap();
    assert_eq!(explicit.lanes, Some(Lanes::Merge));
}

#[test]
fn validation_names_each_problem() {
    let row = |pairs: &[(&str, &str)]| -> SheetRow {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let mut spec = rnaseq_spec();
    spec.paired_only = true;
    let rows = vec![
        row(&[("sample", "a"), ("fastq_1", "s3://b/a.fastq.gz"), ("strandedness", "sideways")]),
        row(&[
            ("sample", ""),
            ("fastq_1", "local/a.fastq.gz"),
            ("fastq_2", "s3://b/a.txt"),
            ("strandedness", "auto"),
        ]),
        row(&[
            ("sample", "c"),
            ("fastq_1", "s3://b/a.fastq.gz"),
            ("extra", "x"),
            ("strandedness", "auto"),
            ("fastq_2", "s3://b/c2.fastq.gz"),
        ]),
    ];
    let msgs: Vec<String> = spec
        .validate(&rows)
        .iter()
        .map(|p| format!("{}:{}:{}", p.row.unwrap(), p.column.clone().unwrap(), p.message))
        .collect();
    assert_eq!(
        msgs,
        [
            "0:strandedness:not one of auto, forward",
            "0:fastq_2:this workflow needs paired-end reads",
            "1:sample:required",
            "1:fastq_1:must be an s3:// path",
            "2:extra:not a column of this samplesheet",
            "2:fastq_1:the same file as row 1",
        ]
    );
    assert_eq!(spec.validate(&[])[0].message, "no samples");
}

#[test]
fn samples_are_counted_by_id_not_by_lane_row() {
    let spec = rnaseq_spec();
    let row = |id: &str| -> SheetRow { [("sample".to_string(), id.to_string())].into() };
    assert_eq!(spec.sample_count(&[row("A"), row("A "), row("B"), row(" ")]), 2);
    let no_id = SheetSpec {
        columns: vec![SheetColumn { name: "file".into(), ..Default::default() }],
        ..Default::default()
    };
    assert_eq!(no_id.sample_count(&[row("A"), row("A")]), 2);
}

#[test]
fn renders_a_cell_quoted_only_where_its_delimiter_needs_it() {
    let spec = rnaseq_spec();
    let row: SheetRow =
        [("sample", "a,b"), ("fastq_1", "s3://b/a.fastq.gz"), ("strandedness", "auto")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    assert_eq!(
        spec.render(std::slice::from_ref(&row)),
        "sample,fastq_1,fastq_2,strandedness\n\"a,b\",s3://b/a.fastq.gz,,auto\n"
    );
    let tsv = SheetSpec { format: SheetFormat::Tsv, ..rnaseq_spec() };
    assert_eq!(
        tsv.render(&[row]),
        "sample\tfastq_1\tfastq_2\tstrandedness\na,b\ts3://b/a.fastq.gz\t\tauto\n"
    );
}

#[test]
fn a_tsv_cell_with_a_quote_tab_or_line_break_is_quoted_and_reads_back() {
    // Rewriting such a cell would change a file's path or a sample's id; quoting keeps it, for the
    // engine's csv reader and for `parse` alike.
    let tsv = SheetSpec { format: SheetFormat::Tsv, ..rnaseq_spec() };
    let row: SheetRow = [
        ("sample", "\"S1"),
        ("fastq_1", "s3://b/a\tb.fastq.gz"),
        ("fastq_2", "s3://b/\"x\".fastq.gz"),
        ("strandedness", "two\nlines"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let text = tsv.render(std::slice::from_ref(&row));
    assert_eq!(
        text,
        "sample\tfastq_1\tfastq_2\tstrandedness\n\
         \"\"\"S1\"\t\"s3://b/a\tb.fastq.gz\"\t\"s3://b/\"\"x\"\".fastq.gz\"\t\"two\nlines\"\n"
    );
    assert_eq!(tsv.parse(&text, SheetFormat::Tsv, 100).unwrap(), [row]);
}

#[test]
fn a_path_column_takes_a_file_or_a_folder() {
    let schema = serde_json::json!({
        "items": {
            "type": "object",
            "properties": {
                "sample": { "type": "string", "meta": ["id"] },
                "reads": { "type": "string", "format": "path" },
                "fastq": { "type": "string", "format": "file-path" }
            }
        }
    });
    let order: Vec<String> =
        ["sample", "reads", "fastq"].iter().map(std::string::ToString::to_string).collect();
    let spec = spec_from_row_schema(&schema, &order).unwrap();
    let column = |name: &str| spec.columns.iter().find(|c| c.name == name).unwrap();
    assert!(!column("sample").file);
    assert!(column("reads").file && column("reads").accepts_folder);
    assert!(column("fastq").file && !column("fastq").accepts_folder);
}

#[test]
fn an_nf_core_row_schema_becomes_a_spec() {
    let schema = serde_json::json!({
        "items": {
            "type": "object",
            "required": ["sample", "fastq_1", "strandedness"],
            "properties": {
                "sample": { "type": "string", "meta": ["id"], "errorMessage": "Sample name must be provided" },
                "fastq_1": { "type": "string", "format": "file-path", "pattern": "^\\S+\\.f(ast)?q\\.gz$" },
                "fastq_2": { "type": "string", "format": "file-path", "pattern": "^\\S+\\.f(ast)?q\\.gz$" },
                "strandedness": { "type": "string", "enum": ["forward", "reverse", "unstranded", "auto"] }
            }
        }
    });
    let order: Vec<String> = ["sample", "fastq_1", "fastq_2", "strandedness"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    let spec = spec_from_row_schema(&schema, &order).unwrap();
    let cols: Vec<(&str, Option<ColumnRole>, bool, bool)> =
        spec.columns.iter().map(|c| (c.name.as_str(), c.role, c.required, c.file)).collect();
    assert_eq!(
        cols,
        [
            ("sample", Some(ColumnRole::SampleId), true, false),
            ("fastq_1", Some(ColumnRole::Read1), true, true),
            ("fastq_2", Some(ColumnRole::Read2), false, true),
            ("strandedness", None, true, false),
        ]
    );
    assert_eq!(spec.columns[1].suffixes, [".fastq.gz", ".fq.gz"]);
    assert_eq!(spec.columns[3].enum_values.as_ref().unwrap().len(), 4);
}

#[test]
fn a_row_schema_pattern_gives_the_endings_it_accepts_or_none() {
    let endings = |p: &str| endings_from_pattern(p);
    assert_eq!(endings(r"^\S+\.f(ast)?q\.gz$"), [".fastq.gz", ".fq.gz"]);
    // Optional gzip: the uncompressed endings are accepted too.
    assert_eq!(endings(r"^\S+\.f(ast)?q(\.gz)?$"), [".fastq.gz", ".fq.gz", ".fastq", ".fq"]);
    assert_eq!(endings(r"^\S+\.fastq$"), [".fastq"]);
    assert_eq!(endings(r"^\S+\.(fastq\.gz|bam)$"), [".fastq.gz", ".bam"]);
    // Not reducible to endings: anything goes, other files too, no sequencing ending, or a
    // pattern the regex crate cannot compile.
    assert!(endings(r"^\S+$").is_empty());
    assert!(endings(r"^\S+\.(fastq\.gz|txt)$").is_empty());
    assert!(endings(r"^\S+\.bed$").is_empty());
    assert!(endings(r"^(?!tmp)\S+\.fastq\.gz$").is_empty());

    let spec = spec_from_row_schema(
        &serde_json::json!({
            "items": { "properties": {
                "fastq_1": { "type": "string", "pattern": "^\\S+\\.f(ast)?q(\\.gz)?$" },
                "sample": { "type": "string", "pattern": "^\\S+$" },
            }}
        }),
        &[],
    )
    .unwrap();
    let fastq = spec.columns.iter().find(|c| c.name == "fastq_1").unwrap();
    assert!(fastq.file);
    let rows = vec![SheetRow::from([
        ("fastq_1".to_string(), "s3://b/A_R1.fastq".to_string()),
        ("sample".to_string(), "A".to_string()),
    ])];
    assert!(spec.validate(&rows).is_empty());
    let sample = spec.columns.iter().find(|c| c.name == "sample").unwrap();
    assert!(!sample.file && sample.suffixes.is_empty());
}

#[test]
fn a_row_schema_s_number_choices_are_text_and_a_null_default_is_none() {
    let spec = spec_from_row_schema(
        &serde_json::json!({
            "items": { "properties": {
                "replicate": { "type": "integer", "enum": [1, 2, 3], "default": 1 },
                "flag": { "type": "boolean", "enum": [true, false, null] },
                "note": { "type": "string", "default": null },
            }}
        }),
        &["replicate".into(), "flag".into(), "note".into()],
    )
    .unwrap();
    assert_eq!(
        spec.columns[0].enum_values.as_deref(),
        Some(&["1".to_string(), "2".into(), "3".into()][..])
    );
    assert_eq!(spec.columns[0].default.as_deref(), Some("1"));
    assert_eq!(
        spec.columns[1].enum_values.as_deref(),
        Some(&["true".to_string(), "false".into()][..])
    );
    assert_eq!(spec.columns[2].default, None);
    let row = SheetRow::from([("replicate".to_string(), "2".to_string())]);
    assert!(spec.validate(&[row]).is_empty());
}

fn cells(rows: &[SheetRow], column: &str) -> Vec<String> {
    rows.iter().map(|r| r.get(column).cloned().unwrap_or_default()).collect()
}

const NF_CORE_DEMO_SHEET: &str = "sample,fastq_1,fastq_2\n\
        SAMPLE1_PE,s3://b/sample1_R1.fastq.gz,s3://b/sample1_R2.fastq.gz\n\
        SAMPLE2_PE,s3://b/sample2_R1.fastq.gz,s3://b/sample2_R2.fastq.gz\n\
        SAMPLE3_SE,s3://b/sample1_R1.fastq.gz,\n\
        SAMPLE3_SE,s3://b/sample2_R1.fastq.gz,\n";

#[test]
fn a_sheet_file_parses_into_rows_by_column_name() {
    let rows = rnaseq_spec().parse(NF_CORE_DEMO_SHEET, SheetFormat::Csv, 100).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["fastq_2"], "s3://b/sample1_R2.fastq.gz");
    assert_eq!(rows[2]["fastq_2"], "");
    assert_eq!(rnaseq_spec().sample_ids(&rows), ["SAMPLE1_PE", "SAMPLE2_PE", "SAMPLE3_SE"]);
    assert_eq!(rnaseq_spec().sample_count(&rows), 3);
}

#[test]
fn quoted_csv_fields_hold_delimiters_quotes_and_line_breaks() {
    let text = "sample,note\n\"a,b\",\"say \"\"hi\"\"\"\n\"c\",\"two\r\nlines\"\nd,mid\"quote\n";
    let rows = rnaseq_spec().parse(text, SheetFormat::Csv, 100).unwrap();
    assert_eq!(cells(&rows, "sample"), ["a,b", "c", "d"]);
    assert_eq!(cells(&rows, "note"), ["say \"hi\"", "two\r\nlines", "mid\"quote"]);
}

#[test]
fn crlf_a_bom_blank_lines_and_a_missing_final_break_are_read() {
    let text = "\u{feff}sample,fastq_1\r\nA,s3://b/a.fq.gz\r\n\r\n,\r\nB,s3://b/b.fq.gz";
    let rows = rnaseq_spec().parse(text, SheetFormat::Csv, 100).unwrap();
    assert_eq!(cells(&rows, "sample"), ["A", "B"]);
    assert_eq!(cells(&rows, "fastq_1"), ["s3://b/a.fq.gz", "s3://b/b.fq.gz"]);
    // Header names are trimmed; a short row leaves its last columns out.
    let rows = rnaseq_spec().parse(" sample ,fastq_1\nA\n\n\n", SheetFormat::Csv, 100).unwrap();
    assert_eq!(rows, [SheetRow::from([("sample".into(), "A".into())])]);
}

#[test]
fn tsv_fields_are_quoted_as_python_s_csv_reader_reads_them() {
    let text = "sample\tfastq_1\n\"A,1\"\ts3://b/a.fq.gz\n\"say \"\"hi\"\"\"\tx\"y\n\"B\tz\"\n\
                \"two\nlines\"\tw\n";
    let rows = rnaseq_spec().parse(text, SheetFormat::Tsv, 100).unwrap();
    // As Python's csv reader with `delimiter='\t'`: a quoted field loses its quotes (a doubled
    // one is one) and holds tabs and line breaks; a quote elsewhere is text.
    assert_eq!(cells(&rows, "sample"), ["A,1", "say \"hi\"", "B\tz", "two\nlines"]);
    assert_eq!(cells(&rows, "fastq_1"), ["s3://b/a.fq.gz", "x\"y", "", "w"]);
    assert_eq!(
        rnaseq_spec().parse("sample\n\"A\tB\n", SheetFormat::Tsv, 100),
        Err(SheetParseError::UnclosedQuote { line: 2 })
    );
    // A cell of two quotes is empty, so a row of them is blank.
    let rows = rnaseq_spec().parse("sample\n\"\"\nA\n", SheetFormat::Tsv, 100).unwrap();
    assert_eq!(cells(&rows, "sample"), ["A"]);
}

#[test]
fn trailing_unnamed_columns_and_empty_cells_are_ignored() {
    let spec = rnaseq_spec();
    // As a spreadsheet exports a sheet with stray columns: trailing delimiters everywhere.
    let rows = spec.parse("sample,fastq_1,,\nA,x,,\nB,y\n", SheetFormat::Csv, 100).unwrap();
    assert_eq!(cells(&rows, "sample"), ["A", "B"]);
    assert_eq!(rows[0].len(), 2, "no column without a name: {:?}", rows[0]);
    let rows = spec.parse("sample\tfastq_1\nA\tx\t\t \n", SheetFormat::Tsv, 100).unwrap();
    assert_eq!(cells(&rows, "fastq_1"), ["x"]);
    // An unnamed column before a named one is refused, naming its place.
    assert_eq!(
        spec.parse("sample,,fastq_1\nA,,x\n", SheetFormat::Csv, 100),
        Err(SheetParseError::UnnamedColumn(2))
    );
    assert_eq!(SheetParseError::UnnamedColumn(2).to_string(), "the header's column 2 has no name");
    // A non-empty cell past the named columns is refused, even under an unnamed one.
    assert_eq!(
        spec.parse("sample,fastq_1,,\nA,x,,y\n", SheetFormat::Csv, 100),
        Err(SheetParseError::TooManyCells { line: 2, cells: 4, columns: 2 })
    );
}

#[test]
fn blank_lines_do_not_count_toward_the_row_cap() {
    let spec = rnaseq_spec();
    let text = "sample\n\n,\nA\n\n\nB\n\n";
    assert_eq!(spec.parse(text, SheetFormat::Csv, 2).unwrap().len(), 2);
    // Past the cap is refused however much follows (the rest is not read).
    let mut long = String::from("sample\n");
    for i in 0..10_000 {
        long.push_str(&format!("S{i}\n"));
    }
    long.push_str("\"unclosed\n");
    assert_eq!(spec.parse(&long, SheetFormat::Csv, 2), Err(SheetParseError::TooManyRows(2)));
}

#[test]
fn a_sheet_that_cannot_name_its_samples_does_not_parse() {
    let spec = rnaseq_spec();
    let parse = |t: &str| spec.parse(t, SheetFormat::Csv, 2);
    assert_eq!(parse(""), Err(SheetParseError::Empty));
    assert_eq!(parse("\u{feff}\n\n"), Err(SheetParseError::Empty));
    assert_eq!(
        parse("id,fastq_1\nA,s3://b/a.fq.gz\n"),
        Err(SheetParseError::MissingColumn("sample".into()))
    );
    assert_eq!(
        parse("sample,sample\nA,B\n"),
        Err(SheetParseError::DuplicateColumn("sample".into()))
    );
    assert_eq!(parse("sample\nA\n\"B\nC,D\n"), Err(SheetParseError::UnclosedQuote { line: 3 }));
    // The line the quote opened on, not the line its record starts on.
    assert_eq!(
        parse("sample,note\nA,1\n\"B\nb\",\"unclosed\n"),
        Err(SheetParseError::UnclosedQuote { line: 4 })
    );
    assert_eq!(
        parse("sample,fastq_1\nA,x\n\"B\nb\",x,extra\n"),
        Err(SheetParseError::TooManyCells { line: 3, cells: 3, columns: 2 })
    );
    // At the cap is fine; one past it is refused, not cut short.
    assert_eq!(parse("sample\nA\nB\n").unwrap().len(), 2);
    assert_eq!(parse("sample\nA\nB\nC\n"), Err(SheetParseError::TooManyRows(2)));
    // A header alone parses, to no rows (and so no sample ids).
    assert_eq!(parse("sample,fastq_1\n"), Ok(Vec::new()));
    // A spec with no sample-id column needs none in the file.
    let no_id = SheetSpec {
        columns: vec![SheetColumn { name: "x".into(), ..Default::default() }],
        ..Default::default()
    };
    assert_eq!(no_id.parse("y\n1\n", SheetFormat::Csv, 2).unwrap().len(), 1);
}

#[test]
fn a_rendered_sheet_parses_back_to_its_rows() {
    let row = |pairs: &[(&str, &str)]| -> SheetRow {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let rows = vec![
        row(&[
            ("sample", "a,b \"quoted\""),
            ("fastq_1", "s3://b/a.fastq.gz"),
            ("fastq_2", ""),
            ("strandedness", "auto"),
        ]),
        row(&[
            ("sample", "multi\nline\r\nid"),
            ("fastq_1", "s3://b/b,1.fastq.gz"),
            ("fastq_2", "s3://b/b_2.fastq.gz"),
            ("strandedness", "forward"),
        ]),
    ];
    for format in [SheetFormat::Csv, SheetFormat::Tsv] {
        let spec = SheetSpec { format, ..rnaseq_spec() };
        let parsed = spec.parse(&spec.render(&rows), format, 100).unwrap();
        assert_eq!(parsed, rows, "{format:?}");
        assert_eq!(spec.sample_ids(&parsed).len(), 2);
    }
}

#[test]
fn the_file_ending_names_the_sheet_format() {
    assert_eq!(SheetFormat::of_uri("s3://b/x/sheet.CSV"), Some(SheetFormat::Csv));
    assert_eq!(SheetFormat::of_uri("s3://b/sheet.tsv"), Some(SheetFormat::Tsv));
    assert_eq!(SheetFormat::of_uri("s3://b/sheet.txt"), None);
    assert_eq!(SheetFormat::of_uri("s3://b/sheet"), None);
    assert_eq!(SheetFormat::of_uri("s3://b/sheet.csv.gz"), None);
}

#[test]
fn a_sheet_file_s_delimiter_is_the_one_its_header_names_columns_with() {
    let csv = rnaseq_spec();
    let tsv = SheetSpec { format: SheetFormat::Tsv, ..rnaseq_spec() };
    // The header decides, over the ending and the declared format.
    let commas = "\u{feff}\nsample,fastq_1\nA,x\n";
    let tabs = "sample\tfastq_1\nA\tx\n";
    assert_eq!(tsv.file_format(commas, "s3://b/sheet.txt"), SheetFormat::Csv);
    assert_eq!(tsv.file_format(commas, "s3://b/sheet.tsv"), SheetFormat::Csv);
    assert_eq!(csv.file_format(tabs, "s3://b/sheet.csv"), SheetFormat::Tsv);
    // A header naming as many either way: the ending, else the declared format.
    let one = "sample\nA\n";
    assert_eq!(csv.file_format(one, "s3://b/sheet.tsv"), SheetFormat::Tsv);
    assert_eq!(tsv.file_format(one, "s3://b/sheet.csv"), SheetFormat::Csv);
    assert_eq!(tsv.file_format(one, "s3://b/sheet"), SheetFormat::Tsv);
    assert_eq!(csv.file_format(one, "s3://b/sheet.txt"), SheetFormat::Csv);
    assert_eq!(csv.file_format("", "s3://b/sheet"), SheetFormat::Csv);
}
