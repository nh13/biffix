//! MultiQC's General Statistics as data: the per-sample values from `multiqc_general_stats.txt`
//! (what the report shows, already scaled) joined to the columns' metadata from
//! `multiqc_data.json`'s `report_general_stats_headers`. Supports MultiQC 1.14–1.35: headers are a
//! list of per-module maps (≤1.2x) or a map of them keyed by module (1.3x); a TSV column is named
//! by the header's `rid` (≥1.2x) or `<namespace>_<rid>` (1.14), and 1.14 leaves a numeric `modify`
//! multiplier (e.g. reads → millions) for the reader to apply.
//!
//! Also FastQC's per-file statuses from `multiqc_fastqc.txt` ([`fastqc_statuses`]), for a run that
//! published MultiQC's data but not FastQC's own zips.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;

use biffix_api::{FastqcBasicStats, FastqcModuleStatus, FastqcStatusSample, QcColumn, QcSample};

/// The key of `multiqc_data.json` holding the General Statistics columns' metadata.
pub(crate) const HEADERS_KEY: &str = "report_general_stats_headers";

/// MultiQC's General Statistics table, in `multiqc_data/`.
pub(crate) const GENERAL_STATS_FILE: &str = "multiqc_general_stats.txt";

/// MultiQC's data dump (holding [`HEADERS_KEY`]), in `multiqc_data/`.
pub(crate) const DATA_JSON_FILE: &str = "multiqc_data.json";

/// A MultiQC table (TSV: `Sample`, then one column per field; MultiQC's `report.write_data_file`)
/// split into its header's column names (after `Sample`) and each row's cells (the sample's name
/// first), blank lines skipped, nothing trimmed. `None` when the text has no header.
fn split_table(tsv: &str) -> Option<(Vec<&str>, impl Iterator<Item = Vec<&str>>)> {
    let mut lines = tsv.lines().filter(|l| !l.trim().is_empty());
    let names = lines.next()?.split('\t').skip(1).collect();
    Some((names, lines.map(|line| line.split('\t').collect())))
}

/// What reading a prefix of `multiqc_data.json` found.
#[derive(Debug, PartialEq)]
pub(crate) enum HeadersScan {
    /// The headers (JSON `null` when the key was absent from a complete document).
    Found(Value),
    /// The prefix ended first: read more.
    NeedMore,
    /// Not a JSON object, or malformed before the key.
    Invalid(String),
}

/// Find [`HEADERS_KEY`] in `bytes` (a prefix of the document), skipping every other value without
/// building it and stopping as soon as the key's value is read.
pub(crate) fn scan_headers(bytes: &[u8]) -> HeadersScan {
    let mut found: Option<Value> = None;
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let result = de::Deserializer::deserialize_map(&mut de, TopLevel { found: &mut found });
    match (found, result) {
        (Some(v), _) => HeadersScan::Found(v),
        (None, Ok(())) => HeadersScan::Found(Value::Null),
        (None, Err(e)) if e.is_eof() || cut_mid_number(bytes, &e) => HeadersScan::NeedMore,
        (None, Err(e)) => HeadersScan::Invalid(e.to_string()),
    }
}

/// Whether `e` is a number cut short by the end of `bytes`: serde_json calls most cut-short
/// documents EOF, but a number ending in `-`, `.`, `e`/`E` or `+` is "invalid number" at the very
/// end instead. An error before the end, or at a last byte no number can hold, is real.
fn cut_mid_number(bytes: &[u8], e: &serde_json::Error) -> bool {
    bytes.last().is_some_and(|b| b.is_ascii_digit() || b"-+.eE".contains(b))
        && error_offset(bytes, e) >= bytes.len()
}

/// The byte offset in `bytes` a serde_json error points at (its 1-based line, and its column: the
/// bytes read on that line).
fn error_offset(bytes: &[u8], e: &serde_json::Error) -> usize {
    let line_start = if e.line() <= 1 {
        0
    } else {
        bytes
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(e.line() - 2)
            .map_or(bytes.len(), |(i, _)| i + 1)
    };
    line_start + e.column()
}

/// The error that ends the scan once the headers are in hand (the rest is never read).
const STOP: &str = "biffix: general stats headers found";

struct TopLevel<'a> {
    found: &'a mut Option<Value>,
}

impl<'de> Visitor<'de> for TopLevel<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a MultiQC data object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == HEADERS_KEY {
                *self.found = Some(map.next_value::<Value>()?);
                return Err(de::Error::custom(STOP));
            }
            map.next_value::<IgnoredAny>()?;
        }
        Ok(())
    }
}

/// One column's metadata, from a header entry.
#[derive(Clone, Debug, Default, PartialEq)]
struct Header {
    column: QcColumn,
    /// A numeric multiplier to apply to the TSV value (MultiQC 1.14).
    modify: Option<f64>,
}

/// The headers by every name a TSV column may carry: `rid`, `clean_rid` and `<namespace>_<rid>`.
fn index_headers(headers: &Value) -> HashMap<String, Header> {
    let modules: Vec<&Value> = match headers {
        Value::Array(list) => list.iter().collect(),
        Value::Object(map) => map.values().collect(),
        _ => Vec::new(),
    };
    let mut index = HashMap::new();
    for module in modules {
        let Some(columns) = module.as_object() else {
            continue;
        };
        for (key, meta) in columns {
            let Some(meta) = meta.as_object() else {
                continue;
            };
            let text = |k: &str| {
                meta.get(k)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let number = |k: &str| meta.get(k).and_then(Value::as_f64);
            let rid = text("rid").unwrap_or_else(|| key.clone());
            let namespace = text("namespace");
            let header = Header {
                column: QcColumn {
                    id: rid.clone(),
                    title: text("title").unwrap_or_else(|| key.clone()),
                    description: text("description"),
                    namespace: namespace.clone(),
                    suffix: meta
                        .get("suffix")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    min: number("min"),
                    max: number("max"),
                    scale: text("scale"),
                    decimals: text("format").and_then(|f| decimals_of(&f)),
                    hidden: meta.get("hidden").and_then(Value::as_bool).unwrap_or(false),
                    cond_formatting_rules: meta
                        .get("cond_formatting_rules")
                        .filter(|v| v.is_object())
                        .cloned(),
                },
                modify: number("modify"),
            };
            let mut names = vec![rid.clone()];
            if let Some(clean) = text("clean_rid") {
                names.push(clean);
            }
            if let Some(ns) = &namespace {
                names.push(format!("{ns}_{rid}"));
            }
            for name in names {
                index.entry(name).or_insert_with(|| header.clone());
            }
        }
    }
    index
}

/// The number of decimals a Python format spec shows (`{:,.1f}` → 1, `{:,.0f}` → 0).
fn decimals_of(format: &str) -> Option<u8> {
    let spec = format.split_once('.')?.1;
    let digits: String = spec.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Join `multiqc_general_stats.txt` to the headers (`Value::Null` when unknown): the columns in the
/// TSV's order and each sample's numeric values. A column without metadata keeps its TSV name.
pub(crate) fn general_stats(tsv: &str, headers: &Value) -> (Vec<QcColumn>, Vec<QcSample>) {
    let index = index_headers(headers);
    let Some((names, rows)) = split_table(tsv) else {
        return (Vec::new(), Vec::new());
    };
    let resolved: Vec<Header> = names
        .iter()
        .map(|name| {
            index.get(*name).cloned().unwrap_or_else(|| Header {
                column: QcColumn {
                    id: (*name).to_string(),
                    title: name.rsplit('-').next().unwrap_or(name).to_string(),
                    ..QcColumn::default()
                },
                modify: None,
            })
        })
        .collect();
    let samples = rows
        .filter_map(|row| {
            let mut cells = row.into_iter();
            let name = cells.next()?.trim().to_string();
            let values: BTreeMap<String, f64> = cells
                .zip(&resolved)
                .filter_map(|(cell, h)| {
                    let v: f64 = cell.trim().parse().ok()?;
                    v.is_finite().then(|| (h.column.id.clone(), h.modify.map_or(v, |m| v * m)))
                })
                .collect();
            Some(QcSample { name, values })
        })
        .collect();
    (resolved.into_iter().map(|h| h.column).collect(), samples)
}

// ── FastQC statuses (`multiqc_fastqc.txt`) ──────────────────────────────────────────────────────

/// The name of MultiQC's FastQC table, in `multiqc_data/`, for a report with one unanchored FastQC
/// section.
pub(crate) const FASTQC_DATA_FILE: &str = "multiqc_fastqc.txt";

/// The stem MultiQC names every FastQC table by (MultiQC 1.x `modules/fastqc/fastqc.py`:
/// `self.write_data_file(dump_data, "multiqc_fastqc")`).
const FASTQC_TABLE_STEM: &str = "multiqc_fastqc";

/// The FastQC section whose table a file of `multiqc_data/` named `name` is (strictly
/// `^multiqc_fastqc(_.+)?\.txt$`): `Some(None)` for [`FASTQC_DATA_FILE`], `Some(Some(anchor))` for
/// `multiqc_fastqc_<anchor>.txt`, `None` for any other file. MultiQC's
/// `BaseMultiqcModule.write_data_file` appends `_<anchor>` when the section's `module_order` entry
/// sets an `anchor` (`multiqc_fastqc_fastqc_raw.txt`), and a repeated unanchored FastQC section
/// gets the anchor `fastqc-1`, `fastqc-2`, … (`report.save_htmlid`), so its table is
/// `multiqc_fastqc_fastqc-1.txt`.
pub(crate) fn fastqc_table_section(name: &str) -> Option<Option<&str>> {
    let rest = name.strip_prefix(FASTQC_TABLE_STEM)?.strip_suffix(".txt")?;
    if name.contains('/') {
        return None;
    }
    if rest.is_empty() {
        return Some(None);
    }
    rest.strip_prefix('_').filter(|anchor| !anchor.is_empty()).map(Some)
}

/// The data format a file of `multiqc_data/` named `name` says MultiQC wrote its tables in, when
/// that is not TSV (`data_format: csv|json|yaml`): `CSV` or `YAML` for a FastQC table
/// (`multiqc_fastqc.csv`), or any of the three for `multiqc_sources.<ext>`, which every MultiQC run
/// writes (MultiQC 1.35 with `data_format: json` writes no `multiqc_fastqc.json`, only the copy
/// inside `multiqc_data.json`).
pub(crate) fn non_tsv_data_format(name: &str) -> Option<&'static str> {
    let (stem, ext) = name.rsplit_once('.')?;
    let format = match ext {
        "csv" => "CSV",
        "json" => "JSON",
        "yaml" => "YAML",
        _ => return None,
    };
    let fastqc_table = stem
        .strip_prefix(FASTQC_TABLE_STEM)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'));
    let general_stats =
        GENERAL_STATS_FILE.strip_suffix(".txt").is_some_and(|general| stem == general);
    (fastqc_table || general_stats || stem == "multiqc_sources").then_some(format)
}

/// Why `what` (the report's General Statistics, its FastQC results) cannot be shown when MultiQC
/// wrote its data as `format` ([`non_tsv_data_format`]): the one wording both readers use.
pub(crate) fn non_tsv_message(format: &str, what: &str) -> String {
    format!(
        "MultiQC wrote its data as {format} (its `data_format`); only TSV is read, so {what} \
         cannot be shown"
    )
}

/// FastQC's modules by the column name MultiQC gives each (the module's name lowercased, spaces as
/// `_`; MultiQC's `fastqc.py`), with the name FastQC itself writes.
const FASTQC_MODULES: &[(&str, &str)] = &[
    ("basic_statistics", "Basic Statistics"),
    ("per_base_sequence_quality", "Per base sequence quality"),
    ("per_tile_sequence_quality", "Per tile sequence quality"),
    ("per_sequence_quality_scores", "Per sequence quality scores"),
    ("per_base_sequence_content", "Per base sequence content"),
    ("per_sequence_gc_content", "Per sequence GC content"),
    ("per_base_n_content", "Per base N content"),
    ("sequence_length_distribution", "Sequence Length Distribution"),
    ("sequence_duplication_levels", "Sequence Duplication Levels"),
    ("overrepresented_sequences", "Overrepresented sequences"),
    ("adapter_content", "Adapter Content"),
    ("kmer_content", "Kmer Content"),
];

/// Whether `cell` is a FastQC status.
fn is_status(cell: &str) -> bool {
    ["pass", "warn", "fail"].iter().any(|s| cell.eq_ignore_ascii_case(s))
}

/// What one column of `multiqc_fastqc.txt` holds.
enum FastqcColumn {
    /// A module's status, named as FastQC names the module.
    Module(String),
    Filename,
    TotalSequences,
    TotalBases,
    PoorQuality,
    SequenceLength,
    PercentGc,
    AvgLength,
    MedianLength,
    Deduplicated,
    /// Anything else (`File type`, `Encoding`, …).
    Other,
}

/// What a column of `multiqc_fastqc.txt` named `name` holds, given its `cells` down the rows. The
/// file is FastQC's basic statistics (`Total Sequences`, `%GC`, …, as FastQC names them, plus
/// MultiQC's own `avg_sequence_length`, `median_sequence_length` and
/// `total_deduplicated_percentage`) then each module's status (MultiQC 1.x `fastqc.py` writes
/// `basic_statistics` updated with `statuses`). A column named for a module this list does not know
/// (a later FastQC's, or Falco's) is a module when every cell it has is a status; a cell of a known
/// module's column that is not a status is skipped by [`fastqc_statuses`].
fn fastqc_column(name: &str, cells: &[&str]) -> FastqcColumn {
    if let Some((_, module)) = FASTQC_MODULES.iter().find(|(key, _)| *key == name) {
        return FastqcColumn::Module((*module).to_string());
    }
    match name {
        "Filename" => return FastqcColumn::Filename,
        "Total Sequences" => return FastqcColumn::TotalSequences,
        "Total Bases" => return FastqcColumn::TotalBases,
        // FastQC 0.11 renamed 0.10's `Filtered Sequences`.
        "Sequences flagged as poor quality" | "Filtered Sequences" => {
            return FastqcColumn::PoorQuality
        }
        "Sequence length" => return FastqcColumn::SequenceLength,
        "%GC" => return FastqcColumn::PercentGc,
        "avg_sequence_length" => return FastqcColumn::AvgLength,
        "median_sequence_length" => return FastqcColumn::MedianLength,
        "total_deduplicated_percentage" => return FastqcColumn::Deduplicated,
        _ => {}
    }
    let filled: Vec<&str> = cells.iter().map(|c| c.trim()).filter(|c| !c.is_empty()).collect();
    if name.is_empty() || filled.is_empty() || !filled.iter().all(|c| is_status(c)) {
        return FastqcColumn::Other;
    }
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    let titled: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => words,
    };
    FastqcColumn::Module(titled)
}

/// A finite number in `cell`.
fn number(cell: &str) -> Option<f64> {
    cell.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A count in `cell` (MultiQC writes `1500.0`).
fn count(cell: &str) -> Option<u64> {
    number(cell).filter(|v| *v >= 0.0 && *v < 1e18).map(|v| v.round() as u64)
}

/// `cell` as text: a whole number without MultiQC's `.0` (`100.0` → `100`), a range as written.
fn text(cell: &str) -> Option<String> {
    let cell = cell.trim();
    if cell.is_empty() {
        return None;
    }
    Some(match number(cell) {
        Some(v) if v.fract() == 0.0 && v.abs() < 1e15 => format!("{v:.0}"),
        _ => cell.to_string(),
    })
}

/// Each FastQC result in MultiQC's `multiqc_fastqc.txt` (a TSV: `Sample`, then FastQC's basic
/// statistics and each module's status), in the file's order, with no `sample` or `section` set:
/// the modules a row has a status for, in column order, and its basic statistics. A row without a
/// name is left out; a cell that does not parse is absent. Empty when the text has no header.
pub(crate) fn fastqc_statuses(tsv: &str) -> Vec<FastqcStatusSample> {
    let Some((names, rows)) = split_table(tsv) else {
        return Vec::new();
    };
    let names: Vec<&str> = names.into_iter().map(str::trim).collect();
    let rows: Vec<(&str, Vec<&str>)> = rows
        .filter_map(|row| {
            let mut cells = row.into_iter().map(str::trim);
            let name = cells.next()?;
            (!name.is_empty()).then(|| (name, cells.collect()))
        })
        .collect();
    let columns: Vec<FastqcColumn> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let cells: Vec<&str> =
                rows.iter().filter_map(|(_, cells)| cells.get(i).copied()).collect();
            fastqc_column(name, &cells)
        })
        .collect();
    rows.into_iter()
        .map(|(name, cells)| {
            let mut filename = None;
            let mut modules = Vec::new();
            let mut stats = FastqcBasicStats::default();
            for (column, &cell) in columns.iter().zip(&cells) {
                match column {
                    FastqcColumn::Module(module) if is_status(cell) => {
                        modules.push(FastqcModuleStatus {
                            name: module.clone(),
                            status: cell.to_string(),
                        });
                    }
                    FastqcColumn::Module(_) | FastqcColumn::Other => {}
                    FastqcColumn::Filename => {
                        filename = Some(cell.to_string()).filter(|c| !c.is_empty());
                    }
                    FastqcColumn::TotalSequences => stats.total_sequences = count(cell),
                    FastqcColumn::TotalBases => stats.total_bases = text(cell),
                    FastqcColumn::PoorQuality => stats.poor_quality = count(cell),
                    FastqcColumn::SequenceLength => stats.sequence_length = text(cell),
                    FastqcColumn::PercentGc => stats.percent_gc = number(cell),
                    FastqcColumn::AvgLength => stats.avg_sequence_length = number(cell),
                    FastqcColumn::MedianLength => stats.median_sequence_length = number(cell),
                    FastqcColumn::Deduplicated => {
                        stats.percent_duplicates =
                            number(cell).filter(|v| (0.0..=100.0).contains(v)).map(|v| 100.0 - v);
                    }
                }
            }
            FastqcStatusSample {
                sample: None,
                section: None,
                fastqc_name: name.to_string(),
                filename,
                modules,
                stats,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A MultiQC ≥1.2x `multiqc_data.json`, trimmed: headers as a list, big plot data after them.
    fn data_json(headers: Value) -> String {
        json!({
            "report_data_sources": {"FastQC": {"all_sections": {"S1": "fastqc/S1_fastqc.zip"}}},
            "report_general_stats_data": [{"S1": {"percent_gc": 43.0}}],
            HEADERS_KEY: headers,
            "report_plot_data": {"huge": vec![1; 1000]},
        })
        .to_string()
    }

    fn fastqc_headers(rid_prefix: &str) -> Value {
        json!({
            "percent_gc": {"title": "GC", "description": "Average % GC content", "min": 0,
                           "max": 100, "suffix": "%", "scale": "PuRd", "format": "{:,.1f}",
                           "namespace": "FastQC", "rid": format!("{rid_prefix}fastqc-percent_gc")},
            "total_sequences": {"title": "Seqs", "suffix": "M", "namespace": "FastQC",
                                "hidden": true, "rid": format!("{rid_prefix}fastqc-total_sequences")}
        })
    }

    #[test]
    fn the_scan_stops_at_the_headers_and_asks_for_more_when_cut_short() {
        let text = data_json(json!([fastqc_headers("")]));
        let end = text.find("report_plot_data").unwrap();
        // Everything up to the plot data is enough: the rest is never needed.
        match scan_headers(&text.as_bytes()[..end]) {
            HeadersScan::Found(v) => assert_eq!(v, json!([fastqc_headers("")])),
            other => panic!("{other:?}"),
        }
        let start = text.find(HEADERS_KEY).unwrap();
        assert_eq!(scan_headers(&text.as_bytes()[..start + 40]), HeadersScan::NeedMore);
        assert_eq!(scan_headers(br#"{"a": 1}"#), HeadersScan::Found(Value::Null));
        assert!(matches!(scan_headers(b"[1, 2]"), HeadersScan::Invalid(_)));
    }

    #[test]
    fn a_prefix_cut_anywhere_asks_for_more_and_only_real_errors_are_invalid() {
        // Every prefix of a document (whatever its last byte: mid-string, mid-literal, a number
        // cut after `-`, `.` or `e`) is NeedMore until the headers are in it.
        let text = format!(
            "{{\"n\": -1.5e+10,\n \"m\": [0.25, -3, 1E-2, true, null],\n \"{HEADERS_KEY}\": \
             {{\"a\": {{\"min\": -0.5}}}}}}"
        );
        // The headers' value ends one byte before the document does.
        let end = text.len() - 1;
        for cut in 1..end {
            let got = scan_headers(&text.as_bytes()[..cut]);
            assert_eq!(got, HeadersScan::NeedMore, "cut at {cut}: {:?}", &text[..cut]);
        }
        for cut in [end, text.len()] {
            assert!(matches!(
                scan_headers(&text.as_bytes()[..cut]),
                HeadersScan::Found(Value::Object(_))
            ));
        }
        // Not JSON (an error page), or malformed before the end: invalid, not "read more".
        for bad in [
            &b"<html>AccessDenied</html>"[..],
            b"{\"a\": 1,, \"b\": 2",
            b"{\"a\": 1.x5, \"b\": ",
            b"{\"a\":\n -x",
        ] {
            assert!(
                matches!(scan_headers(bad), HeadersScan::Invalid(_)),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn columns_join_by_rid_in_both_header_shapes() {
        let tsv = "Sample\tfastqc-percent_gc\tfastqc-total_sequences\n\
                   S1_R1\t43.0\t0.031605\nS1_R2\t44.5\t\n";
        for headers in [
            json!([fastqc_headers("")]),           // MultiQC 1.2x: a list per module
            json!({"fastqc": fastqc_headers("")}), // MultiQC 1.3x: keyed by module
        ] {
            let (columns, samples) = general_stats(tsv, &headers);
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].title, "GC");
            assert_eq!(columns[0].suffix.as_deref(), Some("%"));
            assert_eq!(columns[0].decimals, Some(1));
            assert_eq!((columns[0].min, columns[0].max), (Some(0.0), Some(100.0)));
            assert!(columns[1].hidden);
            assert_eq!(samples[0].name, "S1_R1");
            assert_eq!(samples[0].values["fastqc-total_sequences"], 0.031605);
            // An empty cell is no value, not zero.
            assert!(!samples[1].values.contains_key("fastqc-total_sequences"));
        }
    }

    #[test]
    fn multiqc_1_14_names_columns_by_namespace_and_leaves_the_multiplier() {
        let mut headers = fastqc_headers("mqc-generalstats-");
        headers["total_sequences"]["modify"] = json!(1e-6);
        let tsv = "Sample\tFastQC_mqc-generalstats-fastqc-percent_gc\t\
                   FastQC_mqc-generalstats-fastqc-total_sequences\nS1\t43.0\t31605.0\n";
        let (columns, samples) = general_stats(tsv, &json!([headers]));
        assert_eq!(columns[0].title, "GC");
        assert_eq!(columns[1].id, "mqc-generalstats-fastqc-total_sequences");
        let seqs = samples[0].values["mqc-generalstats-fastqc-total_sequences"];
        assert!((seqs - 0.031605).abs() < 1e-12, "{seqs}");
    }

    #[test]
    fn a_column_without_metadata_keeps_its_name() {
        let (columns, samples) =
            general_stats("Sample\tqualimap-mean_cov\nS1\t30.5\n", &Value::Null);
        assert_eq!(columns[0].id, "qualimap-mean_cov");
        assert_eq!(columns[0].title, "mean_cov");
        assert_eq!(samples[0].values["qualimap-mean_cov"], 30.5);
        assert_eq!(general_stats("", &Value::Null), (vec![], vec![]));
    }
    // ── FastQC statuses ──────────────────────────────────────────────────────────────────────

    /// MultiQC 1.35's own `multiqc_fastqc.txt` for FastQC 0.12.1 on three FASTQs
    /// (`tests/fixtures/README.md`).
    const MULTIQC_FASTQC: &str = include_str!("../../tests/fixtures/multiqc/multiqc_fastqc.txt");

    #[test]
    fn fastqc_statuses_read_multiqc_s_own_file() {
        let got = fastqc_statuses(MULTIQC_FASTQC);
        let names: Vec<&str> = got.iter().map(|s| s.fastqc_name.as_str()).collect();
        assert_eq!(names, ["S10_R1", "S1_R1", "S1_R2"]);
        let s10 = &got[0];
        assert_eq!(s10.sample, None);
        assert_eq!(s10.filename.as_deref(), Some("S10_R1.fastq.gz"));
        let modules: Vec<(&str, &str)> =
            s10.modules.iter().map(|m| (m.name.as_str(), m.status.as_str())).collect();
        assert_eq!(
            modules,
            [
                ("Basic Statistics", "pass"),
                ("Per base sequence quality", "fail"),
                ("Per sequence quality scores", "fail"),
                ("Per base sequence content", "pass"),
                ("Per sequence GC content", "pass"),
                ("Per base N content", "pass"),
                ("Sequence Length Distribution", "pass"),
                ("Sequence Duplication Levels", "pass"),
                ("Overrepresented sequences", "pass"),
                ("Adapter Content", "pass"),
            ]
        );
        assert_eq!(
            s10.stats,
            FastqcBasicStats {
                total_sequences: Some(1000),
                total_bases: Some("100 kbp".into()),
                poor_quality: Some(0),
                sequence_length: Some("100".into()),
                percent_gc: Some(30.0),
                avg_sequence_length: Some(100.0),
                median_sequence_length: Some(100.0),
                percent_duplicates: Some(0.0),
            }
        );
        assert_eq!(got[1].stats.total_sequences, Some(1500));
    }

    #[test]
    fn fastqc_statuses_keep_unknown_modules_and_ranges_and_skip_what_does_not_parse() {
        let tsv = "Sample\tSequence length\tTotal Sequences\tkmer_thing\tEncoding\t%GC\t\
                   per_base_n_content\ttotal_deduplicated_percentage\tmixed\tadapter_content\n\
                   a_1\t35-151\tlots\tWARN\tSanger\tNaN\t\t80.5\tpass\tN/A\n\
                   \tignored\n\
                   b_1\t151\t12\tpass\tSanger\t40\tfail\t\tSanger\n";
        let got = fastqc_statuses(tsv);
        assert_eq!(got.len(), 2, "a row without a name is left out");
        let a = &got[0];
        assert_eq!(a.stats.sequence_length.as_deref(), Some("35-151"));
        assert_eq!(a.stats.total_sequences, None);
        assert_eq!(a.stats.percent_gc, None, "NaN is not a number");
        assert_eq!(a.stats.percent_duplicates, Some(19.5));
        // A column of statuses only is a module; one mixing statuses and other text (`mixed`) is
        // none; an empty status cell, or a known module's cell that is no status (`N/A`), is no
        // module.
        let status = |name: &str, status: &str| FastqcModuleStatus {
            name: name.into(),
            status: status.into(),
        };
        assert_eq!(a.modules, [status("Kmer thing", "WARN")]);
        // A short row has what it has.
        assert_eq!(got[1].stats.total_sequences, Some(12));
        assert_eq!(
            got[1].modules,
            [status("Kmer thing", "pass"), status("Per base N content", "fail")]
        );
    }

    #[test]
    fn fastqc_statuses_read_crlf_a_bom_and_names_with_spaces() {
        let tsv = "\u{feff}Sample\tFilename\tTotal Sequences\tbasic_statistics\r\n\
                   my sample R1\tmy sample R1.fastq.gz\t10.0\tpass\r\n\
                   \r\n\
                   other\tother.fq\t20\tFAIL\r\n";
        let got = fastqc_statuses(tsv);
        let rows: Vec<(&str, Option<&str>, Option<u64>, &str)> = got
            .iter()
            .map(|s| {
                (
                    s.fastqc_name.as_str(),
                    s.filename.as_deref(),
                    s.stats.total_sequences,
                    s.modules[0].status.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("my sample R1", Some("my sample R1.fastq.gz"), Some(10), "pass"),
                ("other", Some("other.fq"), Some(20), "FAIL"),
            ]
        );
    }

    #[test]
    fn fastqc_0_10_s_filtered_sequences_are_the_poor_quality_count() {
        let got = fastqc_statuses("Sample\tFiltered Sequences\nS1\t3.0\n");
        assert_eq!(got[0].stats.poor_quality, Some(3));
    }

    /// MultiQC 1.35's tables for its FastQC module run twice, raw and trimmed reads, each with its
    /// own anchor (`tests/fixtures/README.md`).
    const MULTIQC_FASTQC_RAW: &str =
        include_str!("../../tests/fixtures/multiqc/sections/multiqc_fastqc_fastqc_raw.txt");
    const MULTIQC_FASTQC_TRIMMED: &str =
        include_str!("../../tests/fixtures/multiqc/sections/multiqc_fastqc_fastqc_trimmed.txt");

    #[test]
    fn each_fastqc_section_s_table_reads_alike() {
        let raw = fastqc_statuses(MULTIQC_FASTQC_RAW);
        let trimmed = fastqc_statuses(MULTIQC_FASTQC_TRIMMED);
        let names = |got: &[FastqcStatusSample]| -> Vec<String> {
            got.iter().map(|s| s.fastqc_name.clone()).collect()
        };
        // The same MultiQC names in both: only the section tells them apart.
        assert_eq!(names(&raw), ["S10_R1", "S1_R1", "S1_R2"]);
        assert_eq!(names(&trimmed), names(&raw));
        assert_eq!(raw[1].stats.sequence_length.as_deref(), Some("100"));
        assert_eq!(trimmed[1].stats.sequence_length.as_deref(), Some("80"));
        assert_eq!(trimmed[1].stats.total_bases.as_deref(), Some("120 kbp"));
        let gc = |s: &FastqcStatusSample| {
            s.modules.iter().find(|m| m.name == "Per sequence GC content").map(|m| m.status.clone())
        };
        assert_eq!(gc(&raw[1]).as_deref(), Some("warn"));
        assert_eq!(gc(&trimmed[1]).as_deref(), Some("pass"));
    }

    #[test]
    fn fastqc_tables_are_named_by_their_section() {
        for (name, want) in [
            ("multiqc_fastqc.txt", Some(None)),
            ("multiqc_fastqc_fastqc_raw.txt", Some(Some("fastqc_raw"))),
            ("multiqc_fastqc_fastqc-1.txt", Some(Some("fastqc-1"))),
            ("multiqc_fastqc_.txt", None),
            ("multiqc_fastqc.json", None),
            ("multiqc_fastqcx.txt", None),
            ("multiqc_fastq_screen.txt", None),
            ("multiqc_general_stats.txt", None),
            ("multiqc_fastqc_a/b.txt", None),
        ] {
            assert_eq!(fastqc_table_section(name), want, "{name}");
        }
        for (name, want) in [
            ("multiqc_fastqc.yaml", Some("YAML")),
            ("multiqc_fastqc_fastqc_raw.csv", Some("CSV")),
            ("multiqc_sources.json", Some("JSON")),
            ("multiqc_sources.txt", None),
            ("multiqc_data.json", None),
            ("multiqc_fastqc.txt", None),
            ("multiqc_fastqcx.yaml", None),
            ("multiqc_general_stats.csv", Some("CSV")),
            ("multiqc_general_stats.yaml", Some("YAML")),
            ("multiqc_general_stats.txt", None),
            ("multiqc_general_statsx.csv", None),
        ] {
            assert_eq!(non_tsv_data_format(name), want, "{name}");
        }
    }

    #[test]
    fn fastqc_statuses_of_nothing_are_none() {
        assert!(fastqc_statuses("").is_empty());
        assert!(fastqc_statuses("Sample\tbasic_statistics\n").is_empty());
        assert!(fastqc_statuses("not a table at all").is_empty());
    }
}
