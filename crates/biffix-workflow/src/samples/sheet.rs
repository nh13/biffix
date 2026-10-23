//! A samplesheet as the workflow declares it ([`SheetSpec`]): its columns, checking rows against
//! them, rendering them as the CSV/TSV the engine reads, deriving the spec from a row schema, and
//! the rows detected samples fill ([`detect_for_sheet`]).

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::samples::detect::auto_read;
use crate::samples::names::{ends_with_ignore_case, SEQ_EXTENSIONS};
use crate::samples::{
    detect, stem, DetectFile, DetectRule, DetectedSample, Detection, Lanes, SampleDetectError,
};
use crate::text::basename;

/// What a samplesheet column holds, so detected samples fill it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColumnRole {
    SampleId,
    Read1,
    Read2,
}

/// One column of a samplesheet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetColumn {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ColumnRole>,
    #[serde(default)]
    pub required: bool,
    /// A file path (`s3://`), existence-checked at submit.
    #[serde(default)]
    pub file: bool,
    /// A file column whose cells may also name a folder (a row schema's `format: path`): a cell
    /// with no object there is checked again as a prefix.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_folder: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// File-name endings a file column accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suffixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The file format a samplesheet is written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SheetFormat {
    #[default]
    Csv,
    Tsv,
}

impl SheetFormat {
    pub fn extension(self) -> &'static str {
        match self {
            SheetFormat::Csv => "csv",
            SheetFormat::Tsv => "tsv",
        }
    }
    fn delimiter(self) -> char {
        match self {
            SheetFormat::Csv => ',',
            SheetFormat::Tsv => '\t',
        }
    }
    /// The format a sheet's file name says (`.csv` or `.tsv`, ignoring case), or `None` when its
    /// ending says neither (`.txt` says nothing: either delimiter is common under it).
    pub(crate) fn of_uri(uri: &str) -> Option<SheetFormat> {
        let name = basename(uri);
        if ends_with_ignore_case(name, ".csv") {
            Some(SheetFormat::Csv)
        } else if ends_with_ignore_case(name, ".tsv") {
            Some(SheetFormat::Tsv)
        } else {
            None
        }
    }
}

/// A samplesheet as the workflow declares it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetSpec {
    pub columns: Vec<SheetColumn>,
    #[serde(default)]
    pub format: SheetFormat,
    /// Every sample must have read 2.
    #[serde(default)]
    pub paired_only: bool,
    /// How detection treats several lanes of one sample.
    #[serde(default)]
    pub lanes: Lanes,
}

/// One row, by column name.
pub type SheetRow = BTreeMap<String, String>;

/// A row with every cell empty (or blank): no sample, left out of the sheet.
fn is_empty_row(row: &SheetRow) -> bool {
    row.values().all(|v| v.trim().is_empty())
}

/// A problem with a samplesheet: `row` is 0-based, `None` for the sheet as a whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetProblem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    pub message: String,
}

/// Why a samplesheet file could not be read ([`SheetSpec::parse`]); `line` is 1-based.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SheetParseError {
    #[error("the samplesheet is empty")]
    Empty,
    #[error("a quoted field opened on line {line} is never closed")]
    UnclosedQuote { line: usize },
    #[error("the header names column `{0}` twice")]
    DuplicateColumn(String),
    #[error("the header's column {0} has no name")]
    UnnamedColumn(usize),
    #[error("the header has no `{0}` column")]
    MissingColumn(String),
    #[error("line {line} has {cells} cells; the header has {columns}")]
    TooManyCells { line: usize, cells: usize, columns: usize },
    #[error("more than {0} rows")]
    TooManyRows(usize),
}

/// Split `text` into records of cells, as Python's `csv` reader (and so a Snakemake workflow, or
/// pandas) reads it with this `delimiter` (the `excel` and `excel-tab` dialects, RFC 4180): a
/// record ends at a line break (`\n`, `\r\n` or a lone `\r`) outside quotes, and a field that opens
/// with `"` runs to its closing quote, holding delimiters and line breaks, with `""` for a quote; a
/// quote elsewhere is kept as is. Unlike Python's lenient reader, a quote never closed is an error.
/// Records whose cells are all empty after trimming (blank lines, delimiter-only lines) are
/// dropped; at most `limit` others are kept, the rest of `text` is not read. Each record comes
/// with the line it starts on.
fn split_records(
    text: &str,
    delimiter: char,
    limit: usize,
) -> Result<Vec<(usize, Vec<String>)>, SheetParseError> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut line = 1;
    let mut start = 1;
    // The line the open quoted field opened on; and whether the field so far is only its opening
    // quote's content (a quote that opens a field is only one at the field's start).
    let (mut quoted, mut at_field_start): (Option<usize>, bool) = (None, true);
    let keep = |records: &mut Vec<(usize, Vec<String>)>, start, record: Vec<String>| {
        if !record.iter().all(|c| c.trim().is_empty()) {
            records.push((start, record));
        }
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if records.len() == limit {
            return Ok(records);
        }
        if quoted.is_some() {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = None,
                _ => {
                    if c == '\n' || (c == '\r' && chars.peek() != Some(&'\n')) {
                        line += 1;
                    }
                    field.push(c);
                }
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                quoted = Some(line);
                at_field_start = false;
            }
            c if c == delimiter => {
                record.push(std::mem::take(&mut field));
                at_field_start = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                keep(&mut records, start, std::mem::take(&mut record));
                line += 1;
                start = line;
                at_field_start = true;
            }
            _ => {
                field.push(c);
                at_field_start = false;
            }
        }
    }
    if let Some(line) = quoted {
        return Err(SheetParseError::UnclosedQuote { line });
    }
    // A last record without a line break.
    if records.len() < limit && (!field.is_empty() || !record.is_empty() || !at_field_start) {
        record.push(field);
        keep(&mut records, start, record);
    }
    Ok(records)
}

impl SheetSpec {
    /// The format to read a sheet file `text` named `uri` in, chosen much as the launch form's
    /// import (`readSheetTable` in `ui/src/lib/launch/samplesheet/sheet.ts`) chooses it: the
    /// delimiter whose first (non-blank) line names more of the declared columns. When neither
    /// names more, the file's ending (`SheetFormat::of_uri`: `.csv` or `.tsv`), else the
    /// declared [`SheetSpec::format`]. (The form's import falls back instead to the delimiter that
    /// splits the header into more columns, then the declared format.)
    pub fn file_format(&self, text: &str, uri: &str) -> SheetFormat {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let matches = |format: SheetFormat| {
            split_records(text, format.delimiter(), 1).ok().and_then(|mut r| r.pop()).map_or(
                0,
                |(_, header)| {
                    header
                        .iter()
                        .filter(|h| self.columns.iter().any(|c| c.name == h.trim()))
                        .count()
                },
            )
        };
        let (csv, tsv) = (matches(SheetFormat::Csv), matches(SheetFormat::Tsv));
        match csv.cmp(&tsv) {
            std::cmp::Ordering::Greater => SheetFormat::Csv,
            std::cmp::Ordering::Less => SheetFormat::Tsv,
            std::cmp::Ordering::Equal => SheetFormat::of_uri(uri).unwrap_or(self.format),
        }
    }

    /// Read a samplesheet file as [`SheetSpec::render`] writes it (and as people write them): a
    /// header row, then a row per line, each by its header's column names (every column of the
    /// file, declared or not; a short row leaves its last columns out). A UTF-8 BOM is skipped and
    /// records whose cells are all empty after trimming (blank lines, `,`-only lines) are no rows.
    /// `format` gives the delimiter; either way a field may be quoted, as Python's `csv` reader
    /// reads it (`split_records`). As spreadsheet exports write them, trailing header
    /// cells with no name are no columns, and a row may run past the named columns with empty
    /// cells. Fails on an unnamed column before a named one, a column named twice, a header
    /// without the sample-id column (when the spec declares one), a row with a non-empty cell
    /// past the named columns, or more than `max_rows` rows (reading no further than one past).
    pub fn parse(
        &self,
        text: &str,
        format: SheetFormat,
        max_rows: usize,
    ) -> Result<Vec<SheetRow>, SheetParseError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        // The header, the rows the cap allows and one more, to tell a sheet past the cap.
        let limit = max_rows.saturating_add(2);
        let mut records = split_records(text, format.delimiter(), limit)?.into_iter();
        let (_, header) = records.next().ok_or(SheetParseError::Empty)?;
        let mut header: Vec<String> = header.iter().map(|h| h.trim().to_string()).collect();
        while header.last().is_some_and(String::is_empty) {
            header.pop();
        }
        let mut names = BTreeSet::new();
        for (i, h) in header.iter().enumerate() {
            if h.is_empty() {
                return Err(SheetParseError::UnnamedColumn(i + 1));
            }
            if !names.insert(h.as_str()) {
                return Err(SheetParseError::DuplicateColumn(h.clone()));
            }
        }
        if let Some(id) = self.column(ColumnRole::SampleId) {
            if !names.contains(id.name.as_str()) {
                return Err(SheetParseError::MissingColumn(id.name.clone()));
            }
        }
        let mut rows = Vec::new();
        for (line, mut cells) in records {
            if let Some(last) = cells.iter().rposition(|c| !c.trim().is_empty()) {
                if last >= header.len() {
                    return Err(SheetParseError::TooManyCells {
                        line,
                        cells: last + 1,
                        columns: header.len(),
                    });
                }
            }
            if rows.len() == max_rows {
                return Err(SheetParseError::TooManyRows(max_rows));
            }
            cells.truncate(header.len());
            rows.push(header.iter().cloned().zip(cells).collect());
        }
        Ok(rows)
    }

    fn column(&self, role: ColumnRole) -> Option<&SheetColumn> {
        self.columns.iter().find(|c| c.role == Some(role))
    }

    /// How many samples `rows` hold: distinct sample ids as [`SheetSpec::render`] writes them
    /// (trimmed; a sample sequenced on several lanes has a row per lane pair), or the number of
    /// rows when there is no such column. A row with every cell empty is no sample.
    pub fn sample_count(&self, rows: &[SheetRow]) -> u32 {
        match self.column(ColumnRole::SampleId) {
            Some(_) => self.sample_ids(rows).len() as u32,
            None => rows.iter().filter(|r| !is_empty_row(r)).count() as u32,
        }
    }

    /// The distinct sample ids `rows` hold (trimmed, in first-seen order), from the sample-id
    /// column; empty when the sheet has none.
    pub fn sample_ids(&self, rows: &[SheetRow]) -> Vec<String> {
        let Some(col) = self.column(ColumnRole::SampleId) else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        rows.iter()
            .filter(|r| !is_empty_row(r))
            .filter_map(|r| r.get(&col.name))
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && seen.insert(v.to_string()))
            .map(str::to_string)
            .collect()
    }

    /// Rows for detected samples: the id, read 1 and read 2 in their columns (a merged-lanes sample
    /// becomes one row per lane pair, as nf-core samplesheets expect), and each other column's
    /// default. Mates pair within a lane: a lane short of a read leaves that cell empty rather than
    /// borrowing another lane's file.
    pub fn rows_from(&self, samples: &[DetectedSample]) -> Vec<SheetRow> {
        let mut rows = Vec::new();
        for s in samples {
            // A file's lane is read off its name, as detection read it.
            let lane = |uri: &String| auto_read(stem(basename(uri))).lane;
            let mut by_lane: BTreeMap<Option<String>, (Vec<&String>, Vec<&String>)> =
                BTreeMap::new();
            for f in &s.read1 {
                by_lane.entry(lane(f)).or_default().0.push(f);
            }
            for f in &s.read2 {
                by_lane.entry(lane(f)).or_default().1.push(f);
            }
            if by_lane.is_empty() {
                by_lane.insert(None, (Vec::new(), Vec::new()));
            }
            for (read1, read2) in by_lane.values() {
                for i in 0..read1.len().max(read2.len()).max(1) {
                    let mut row = SheetRow::new();
                    for c in &self.columns {
                        let value = match c.role {
                            Some(ColumnRole::SampleId) => Some(s.id.clone()),
                            Some(ColumnRole::Read1) => read1.get(i).map(|f| (*f).clone()),
                            Some(ColumnRole::Read2) => read2.get(i).map(|f| (*f).clone()),
                            None => c.default.clone(),
                        };
                        row.insert(c.name.clone(), value.unwrap_or_default());
                    }
                    rows.push(row);
                }
            }
        }
        rows
    }

    /// Every problem with `rows`: an unknown column, a required cell left empty, a value outside
    /// its choices, a file that is not an `s3://` path or has the wrong ending, a sample without
    /// read 2 when the workflow needs pairs, and the same read file twice (as read 1 or read 2, in
    /// two rows or both reads of one; another file column, such as a shared panel, may repeat).
    /// A row with every cell empty is left out of the sheet ([`SheetSpec::render`]), so it has no
    /// problems of its own.
    pub fn validate(&self, rows: &[SheetRow]) -> Vec<SheetProblem> {
        let mut problems = Vec::new();
        if rows.iter().all(is_empty_row) {
            problems.push(SheetProblem { row: None, column: None, message: "no samples".into() });
        }
        let known: std::collections::BTreeSet<&str> =
            self.columns.iter().map(|c| c.name.as_str()).collect();
        // Each read file's last row and column, so a repeat names the one before it.
        let mut seen_files: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
        for (i, row) in rows.iter().enumerate() {
            if is_empty_row(row) {
                continue;
            }
            let at = |column: &str, message: String| SheetProblem {
                row: Some(i),
                column: Some(column.to_string()),
                message,
            };
            for name in row.keys() {
                if !known.contains(name.as_str()) {
                    problems.push(at(name, "not a column of this samplesheet".into()));
                }
            }
            for c in &self.columns {
                let value = row.get(&c.name).map(|v| v.trim()).unwrap_or("");
                if value.is_empty() {
                    if c.required {
                        problems.push(at(&c.name, "required".into()));
                    }
                    continue;
                }
                if let Some(allowed) = &c.enum_values {
                    if !allowed.iter().any(|a| a == value) {
                        problems.push(at(&c.name, format!("not one of {}", allowed.join(", "))));
                    }
                }
                if c.file {
                    if !value.starts_with("s3://") {
                        problems.push(at(&c.name, "must be an s3:// path".into()));
                    } else if !c.suffixes.is_empty()
                        && !c
                            .suffixes
                            .iter()
                            .any(|s| value.to_lowercase().ends_with(&s.to_lowercase()))
                    {
                        problems
                            .push(at(&c.name, format!("must end in {}", c.suffixes.join(" or "))));
                    }
                    // Reads are each sample's own: the same read file twice (in any read column)
                    // is a mistake, while a shared file (a panel, a reference) repeats by design.
                    if matches!(c.role, Some(ColumnRole::Read1 | ColumnRole::Read2)) {
                        if let Some((row, column)) = seen_files.insert(value, (i, &c.name)) {
                            problems.push(at(
                                &c.name,
                                if row == i {
                                    format!("the same file as {column}")
                                } else {
                                    format!("the same file as row {}", row + 1)
                                },
                            ));
                        }
                    }
                }
            }
            if self.paired_only {
                if let Some(r2) = self.column(ColumnRole::Read2) {
                    if row.get(&r2.name).is_none_or(|v| v.trim().is_empty()) {
                        problems.push(at(&r2.name, "this workflow needs paired-end reads".into()));
                    }
                }
            }
        }
        problems
    }

    /// The sheet as the engine reads it: a header of the declared columns, then one line per row
    /// (a row with every cell empty is left out), each cell trimmed. A cell holding the delimiter,
    /// a quote or a line break is quoted as Python's `csv` writer quotes it, so the engine's `csv`
    /// reader (or pandas) and [`SheetSpec::parse`] read back the value unchanged.
    pub fn render(&self, rows: &[SheetRow]) -> String {
        let delimiter = self.format.delimiter();
        let cell = |v: &str| crate::text::quote_field(v, delimiter);
        let separator = delimiter.to_string();
        let mut out = String::new();
        let header: Vec<String> = self.columns.iter().map(|c| cell(&c.name)).collect();
        out.push_str(&header.join(&separator));
        out.push('\n');
        for row in rows.iter().filter(|r| !is_empty_row(r)) {
            let line: Vec<String> = self
                .columns
                .iter()
                .map(|c| cell(row.get(&c.name).map(|v| v.trim()).unwrap_or("")))
                .collect();
            out.push_str(&line.join(&separator));
            out.push('\n');
        }
        out
    }
}

/// Detection as the launch form asks for it (`POST /samples/detect`, whose wire types are
/// `biffix_api::{SampleDetectRequest, DetectedSamples}`): samples detected in `files` by `rule`
/// (under the samplesheet's lane rule unless the caller chose one) and, given the workflow's
/// `samplesheet`, its rows for them ([`SheetSpec::rows_from`]; empty without one).
pub fn detect_for_sheet(
    files: &[DetectFile],
    rule: &DetectRule,
    samplesheet: Option<&SheetSpec>,
) -> Result<(Detection, Vec<SheetRow>), SampleDetectError> {
    let mut rule = rule.clone();
    // The workflow says what it does with lanes unless the caller chose (any choice, `merge` too).
    if rule.lanes.is_none() {
        rule.lanes = samplesheet.map(|s| s.lanes);
    }
    let detection = detect(files, &rule)?;
    let rows = samplesheet.map(|s| s.rows_from(&detection.samples)).unwrap_or_default();
    Ok((detection, rows))
}

/// The role a column name suggests (`fastq_1` → read 1), for sheets that declare none.
pub(crate) fn role_from_name(name: &str) -> Option<ColumnRole> {
    match name.to_lowercase().replace('-', "_").as_str() {
        "sample" | "sample_id" | "sampleid" | "id" | "sample_name" => Some(ColumnRole::SampleId),
        "fastq_1" | "fastq1" | "r1" | "read1" | "read_1" | "fq1" | "fastq_r1" => {
            Some(ColumnRole::Read1)
        }
        "fastq_2" | "fastq2" | "r2" | "read2" | "read_2" | "fq2" | "fastq_r2" => {
            Some(ColumnRole::Read2)
        }
        _ => None,
    }
}

/// [`spec_from_row_schema`] from the schema file's text, keeping its column order.
pub(crate) fn spec_from_row_schema_text(text: &str) -> Result<Option<SheetSpec>, String> {
    let yaml: serde_norway::Value = serde_norway::from_str(text).map_err(|e| e.to_string())?;
    let order =
        crate::formschema::mapping_keys(yaml.get("items").and_then(|i| i.get("properties")));
    let value: serde_json::Value = serde_norway::from_value(yaml).map_err(|e| e.to_string())?;
    Ok(spec_from_row_schema(&value, &order))
}

/// Endings that show a pattern accepts more than sequencing files: a pattern that takes any of
/// them is not reduced to endings.
const OTHER_ENDINGS: [&str; 16] = [
    ".biffix-any",
    ".txt",
    ".csv",
    ".tsv",
    ".json",
    ".bed",
    ".vcf",
    ".vcf.gz",
    ".gz",
    ".bz2",
    ".fastq.bz2",
    ".zst",
    ".bai",
    ".crai",
    ".sam",
    ".fa",
];

/// The sequencing-file endings a row schema's `pattern` accepts, read by matching the pattern
/// itself against a file with each ending: `^\S+\.f(ast)?q\.gz$` gives `.fastq.gz` and `.fq.gz`,
/// and `^\S+\.f(ast)?q(\.gz)?$` also `.fastq` and `.fq`. When endings cannot say what the pattern
/// checks (it does not compile, accepts no sequencing ending, or accepts other files too), there
/// are none, and the ending is left to the workflow's own check.
pub(super) fn endings_from_pattern(pattern: &str) -> Vec<String> {
    let Ok(re) = Regex::new(pattern) else {
        return Vec::new();
    };
    let accepts = |ending: &str| {
        ["s3://bucket/sample", "sample"].iter().any(|base| re.is_match(&format!("{base}{ending}")))
    };
    if OTHER_ENDINGS.iter().any(|e| accepts(e)) {
        return Vec::new();
    }
    SEQ_EXTENSIONS.iter().filter(|e| accepts(e)).map(std::string::ToString::to_string).collect()
}

/// A samplesheet spec from an nf-schema row schema (`assets/schema_input.json`): `items.properties`
/// are the columns in file order, `items.required` the required ones; a `file-path` format makes a
/// file column; a `meta` of `id` or a column name like `fastq_1` gives the role; an `enum` gives the
/// choices (a number or boolean as its text); a `pattern` gives the sequencing-file endings it
/// accepts ([`endings_from_pattern`]); a `default` other than `null` fills new rows. `order` is the
/// key order of `items.properties` (JSON parsing loses it).
pub(crate) fn spec_from_row_schema(
    schema: &serde_json::Value,
    order: &[String],
) -> Option<SheetSpec> {
    use crate::formschema::value_key;
    let items = schema.get("items")?;
    let props = items.get("properties")?.as_object()?;
    let required: Vec<&str> = items
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let mut names: Vec<&String> = props.keys().collect();
    names.sort_by_key(|n| order.iter().position(|o| o == *n).unwrap_or(usize::MAX));
    let columns = names
        .into_iter()
        .map(|name| {
            let p = &props[name];
            let text = |k: &str| p.get(k).and_then(|v| v.as_str());
            let is_id_meta = p
                .get("meta")
                .and_then(|m| m.as_array())
                .is_some_and(|m| m.iter().any(|v| v.as_str() == Some("id")));
            let suffixes = text("pattern").map(endings_from_pattern).unwrap_or_default();
            SheetColumn {
                name: name.clone(),
                role: if is_id_meta { Some(ColumnRole::SampleId) } else { role_from_name(name) },
                required: required.contains(&name.as_str()),
                accepts_folder: text("format") == Some("path"),
                file: matches!(text("format"), Some("file-path" | "path")) || !suffixes.is_empty(),
                // A cell is text, so a number or boolean choice is compared as its text (`2`,
                // `true`), as the form's params are; `null` (a nullable enum) is no choice.
                enum_values: p.get("enum").and_then(|e| e.as_array()).map(|a| {
                    a.iter()
                        .filter(|v| v.is_string() || v.is_number() || v.is_boolean())
                        .map(value_key)
                        .collect()
                }),
                default: p.get("default").filter(|d| !d.is_null()).map(value_key),
                suffixes,
                description: text("errorMessage").or(text("description")).map(String::from),
            }
        })
        .collect();
    Some(SheetSpec { columns, ..SheetSpec::default() })
}
