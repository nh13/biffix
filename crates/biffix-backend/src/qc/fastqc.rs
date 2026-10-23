//! FastQC results as data: the `fastqc_data.txt` inside a pipeline's own `{stem}_fastqc.zip`, parsed
//! into its modules for the QC viewer. biffix never runs FastQC; it reads what the pipeline wrote.
//!
//! The file is a series of sections, one per module (tab-separated; shown here with spaces):
//!
//! ```text
//! ##FastQC  0.12.1
//! >>Sequence Duplication Levels  pass
//! #Total Deduplicated Percentage  91.8
//! #Duplication Level  Percentage of total
//! 1  84.15
//! >>END_MODULE
//! ```
//!
//! The parser is generic over modules (a FastQC that adds one needs no change here): the last `#`
//! line before a module's data is its column header, earlier `#` lines are `key<TAB>value` extras.

use std::io::Read;

use biffix_api::FastqcModule;

/// Rows kept per module; a module with more (a long overrepresented-sequences list) is marked
/// `truncated`.
pub(crate) const MAX_ROWS_PER_MODULE: usize = 2000;

/// The largest `fastqc_data.txt` read out of a zip, decompressed (a few hundred KB in practice), so a
/// zip bomb costs at most this much memory.
pub(crate) const MAX_DATA_BYTES: u64 = 16 << 20;

/// Modules kept (FastQC writes about a dozen); later ones are dropped and the data marked
/// `truncated`.
const MAX_MODULES: usize = 64;
/// Cells kept per row, and column names per header (FastQC's widest table has 7 or so).
const MAX_COLUMNS: usize = 64;
/// `#` lines kept per module (its column header and its `key<TAB>value` extras).
const MAX_HASH_LINES: usize = 64;
/// Cells kept in all, and the bytes of text they hold: a bound on what the parse allocates and on
/// the JSON the API returns (a Lambda response is at most 6 MB), whatever the file holds. Real
/// FastQC data is a few thousand cells and well under 1 MB.
const MAX_CELLS: usize = 200_000;
const MAX_CELL_BYTES: usize = 4 << 20;

/// FastQC's zip ending, as FastQC writes it (lowercase).
pub(crate) const ZIP_SUFFIX: &str = "_fastqc.zip";
/// FastQC's HTML report ending, as FastQC writes it (lowercase).
const HTML_SUFFIX: &str = "_fastqc.html";

/// The file inside a FastQC zip that holds the data (`{stem}_fastqc/fastqc_data.txt`).
const DATA_FILE: &str = "fastqc_data.txt";

/// A parsed `fastqc_data.txt`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FastqcData {
    /// The version on the `##FastQC` line, when there is one.
    pub(crate) version: Option<String>,
    pub(crate) modules: Vec<FastqcModule>,
    /// Modules past [`MAX_MODULES`] were dropped, or the cell budget ran out (the module it ran out
    /// in is marked `truncated` too).
    pub(crate) truncated: bool,
}

/// Why a FastQC zip could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FastqcError {
    #[error("not a readable zip: {0}")]
    Zip(String),
    #[error("the zip holds no fastqc_data.txt")]
    NoData,
    #[error("fastqc_data.txt is larger than {MAX_DATA_BYTES} bytes")]
    TooLarge,
    #[error("fastqc_data.txt has no FastQC modules")]
    NoModules,
}

/// The FastQC data in the zip `bytes`: its `fastqc_data.txt` (the shallowest, when a zip somehow
/// holds several), decompressed up to [`MAX_DATA_BYTES`] and parsed.
///
/// A zip64 archive is refused before the zip reader sees it ([`is_zip64`]): the reader sizes its
/// entry table from the archive's claimed entry count, which zip64 lets a few-MB file set to
/// millions (GBs of allocation), while a plain zip's count is at most 65,535. FastQC never writes
/// zip64.
pub(crate) fn from_zip(bytes: &[u8]) -> Result<FastqcData, FastqcError> {
    if is_zip64(bytes) {
        return Err(FastqcError::Zip("a zip64 archive (FastQC writes none)".into()));
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| FastqcError::Zip(e.to_string()))?;
    let names = archive
        .file_names()
        .map(|name| name.map(std::borrow::Cow::into_owned))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| FastqcError::Zip(e.to_string()))?;
    let name = names
        .into_iter()
        .filter(|n| n == DATA_FILE || n.ends_with(&format!("/{DATA_FILE}")))
        .min_by_key(|n| (n.matches('/').count(), n.clone()))
        .ok_or(FastqcError::NoData)?;
    let file = archive.by_name(&name).map_err(|e| FastqcError::Zip(e.to_string()))?;
    // The declared size is the zip's claim, not a bound: read one byte past the cap to tell.
    let mut data = Vec::new();
    file.take(MAX_DATA_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| FastqcError::Zip(e.to_string()))?;
    if data.len() as u64 > MAX_DATA_BYTES {
        return Err(FastqcError::TooLarge);
    }
    let parsed = parse(&String::from_utf8_lossy(&data));
    if parsed.modules.is_empty() {
        return Err(FastqcError::NoModules);
    }
    Ok(parsed)
}

/// Whether the zip `bytes` is (or claims to be) a zip64 archive, by the end-of-central-directory
/// records the zip reader could take: every `PK\x05\x06` in the last 22 + 65,535 bytes (a record
/// and the longest comment, where the one the reader takes must start) that holds a zip64 marker
/// (`0xFFFF` in a count or disk number, `0xFFFFFFFF` as the directory's size or offset) or has a
/// zip64 locator (`PK\x06\x07`) right before it, where the reader looks for one. Only those
/// structures are read, never the entries' compressed data before them, which holds any four bytes
/// by chance (a FastQC zip's images); and every candidate is checked, so a decoy record cannot
/// hide a zip64 one behind it.
fn is_zip64(bytes: &[u8]) -> bool {
    const EOCD: &[u8] = b"PK\x05\x06";
    const LOCATOR: &[u8] = b"PK\x06\x07";
    const EOCD_LEN: usize = 22;
    const LOCATOR_LEN: usize = 20;
    let tail = bytes.len().saturating_sub(EOCD_LEN + usize::from(u16::MAX));
    (tail..bytes.len()).filter(|&at| bytes[at..].starts_with(EOCD)).any(|at| {
        let record = &bytes[at..];
        let u16_at = |o: usize| record.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at =
            |o: usize| record.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let marked = [4, 6, 8, 10].iter().any(|&o| u16_at(o) == Some(u16::MAX))
            || [12, 16].iter().any(|&o| u32_at(o) == Some(u32::MAX));
        let located = at.checked_sub(LOCATOR_LEN).is_some_and(|l| bytes[l..].starts_with(LOCATOR));
        marked || located
    })
}

/// What the parse may still keep ([`MAX_CELLS`] cells, [`MAX_CELL_BYTES`] bytes of them).
struct Budget {
    cells: usize,
    bytes: usize,
}

impl Budget {
    /// Take `cells` cells of `bytes` bytes from the budget, when it holds them.
    fn take(&mut self, cells: usize, bytes: usize) -> bool {
        if cells > self.cells || bytes > self.bytes {
            return false;
        }
        self.cells -= cells;
        self.bytes -= bytes;
        true
    }

    /// `line`'s first [`MAX_COLUMNS`] tab-separated cells, when the budget holds them (and takes
    /// them from it); `None` when it does not. The bool says whether cells past the cap were cut.
    fn cells(&mut self, line: &str) -> Option<(Vec<String>, bool)> {
        let cut = line.split('\t').nth(MAX_COLUMNS).is_some();
        let cells: Vec<&str> = line.split('\t').take(MAX_COLUMNS).collect();
        let bytes: usize = cells.iter().map(|c| c.len()).sum();
        if !self.take(cells.len(), bytes) {
            return None;
        }
        Some((cells.into_iter().map(str::to_string).collect(), cut))
    }
}

/// Parse a `fastqc_data.txt`. Lenient: CRLF line ends, blank lines and lines outside a module are
/// ignored, and a module the file ends inside (no `>>END_MODULE`) is kept as far as it goes.
/// Bounded whatever the text: at most [`MAX_MODULES`] modules, [`MAX_COLUMNS`] cells a row,
/// [`MAX_ROWS_PER_MODULE`] rows and [`MAX_HASH_LINES`] `#` lines a module, and [`MAX_CELLS`] cells
/// ([`MAX_CELL_BYTES`] bytes) in all; what is cut is marked `truncated`.
pub(crate) fn parse(text: &str) -> FastqcData {
    let mut version = None;
    let mut modules = Vec::new();
    let mut truncated = false;
    let mut budget = Budget { cells: MAX_CELLS, bytes: MAX_CELL_BYTES };
    // The module being read, and its `#` lines so far (the last one is the header).
    let mut current: Option<(FastqcModule, Vec<String>)> = None;
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("##FastQC") {
            version = Some(rest.trim().to_string()).filter(|v| !v.is_empty());
        } else if line.starts_with(">>END_MODULE") {
            modules.extend(current.take().map(finish));
        } else if let Some(header) = line.strip_prefix(">>") {
            // A module starts (closing one left open).
            modules.extend(current.take().map(finish));
            if modules.len() >= MAX_MODULES {
                truncated = true;
                continue;
            }
            let (name, status) = header.split_once('\t').unwrap_or((header, ""));
            let (name, status) = (name.trim(), status.trim());
            if !budget.take(2, name.len() + status.len()) {
                truncated = true;
                continue;
            }
            current = Some((
                FastqcModule {
                    name: name.to_string(),
                    status: status.to_ascii_lowercase(),
                    columns: Vec::new(),
                    rows: Vec::new(),
                    extra: Vec::new(),
                    truncated: false,
                },
                Vec::new(),
            ));
        } else if let Some((module, hashes)) = current.as_mut() {
            if let Some(hash) = line.strip_prefix('#') {
                if hashes.len() + module.extra.len() >= MAX_HASH_LINES {
                    module.truncated = true;
                    continue;
                }
                let Some((cells, cut)) = budget.cells(hash) else {
                    module.truncated = true;
                    truncated = true;
                    continue;
                };
                module.truncated |= cut;
                // What was kept of it (a line cut at MAX_COLUMNS cells stays cut).
                let hash = cells.join("\t");
                // A `#` line after the data started is not a header (FastQC writes none); keep it
                // as an extra rather than lose it.
                if module.rows.is_empty() {
                    hashes.push(hash);
                } else {
                    module.extra.push(key_value(&hash));
                }
            } else if !line.trim().is_empty() {
                if module.rows.len() >= MAX_ROWS_PER_MODULE {
                    module.truncated = true;
                    continue;
                }
                match budget.cells(line) {
                    Some((cells, cut)) => {
                        module.truncated |= cut;
                        module.rows.push(cells);
                    }
                    None => {
                        module.truncated = true;
                        truncated = true;
                    }
                }
            }
        }
    }
    modules.extend(current.take().map(finish));
    FastqcData { version, modules, truncated }
}

/// A module with its `#` lines placed: the last is the column header, the ones before are extras.
fn finish((mut module, mut hashes): (FastqcModule, Vec<String>)) -> FastqcModule {
    if let Some(header) = hashes.pop() {
        module.columns = header.split('\t').map(str::to_string).collect();
    }
    let mut extra: Vec<[String; 2]> = hashes.iter().map(|h| key_value(h)).collect();
    extra.append(&mut module.extra);
    module.extra = extra;
    module
}

/// `key<TAB>value` (the value empty when there is no tab).
fn key_value(line: &str) -> [String; 2] {
    let (k, v) = line.split_once('\t').unwrap_or((line, ""));
    [k.trim().to_string(), v.trim().to_string()]
}

// ── FastQC's file names: the one place biffix knows them ─────────────────────────────────────

/// The name FastQC gives the zip for an input named `name`: FastQC's own rule (in its order, each
/// once, case-sensitive) strips `.gz`, `.bz2`, `.txt`, `.fastq`, `.fq`, `.csfastq`, `.sam`, `.bam`
/// and `.ubam`, then appends `_fastqc.zip`.
pub(crate) fn zip_name_for(name: &str) -> String {
    let mut stem = name;
    for ext in [".gz", ".bz2", ".txt", ".fastq", ".fq", ".csfastq", ".sam", ".bam", ".ubam"] {
        stem = stem.strip_suffix(ext).unwrap_or(stem);
    }
    format!("{stem}{ZIP_SUFFIX}")
}

/// Whether `name` is a FASTQ FastQC reads (`.fastq`, `.fq` or `.csfastq`, plain, `.gz` or `.bz2`;
/// any case): the inputs whose zip [`zip_name_for`] names. A BAM is an alignment, not reads.
pub(crate) fn is_fastq(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".gz").or_else(|| lower.strip_suffix(".bz2")).unwrap_or(&lower);
    [".fastq", ".fq", ".csfastq"].iter().any(|ext| stem.len() > ext.len() && stem.ends_with(ext))
}

/// `name` without `suffix` (matched in any case), when what is left is not empty.
fn stem_of<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = name.len().checked_sub(suffix.len()).filter(|cut| *cut > 0)?;
    name.get(cut..).filter(|end| end.eq_ignore_ascii_case(suffix)).map(|_| &name[..cut])
}

/// Whether `name` is a FastQC zip (`{stem}_fastqc.zip`, any case, a non-empty stem).
pub(crate) fn is_zip(name: &str) -> bool {
    stem_of(name, ZIP_SUFFIX).is_some()
}

/// The `_fastqc.zip` beside a FastQC HTML report named `name` (`S1_fastqc.html` →
/// `S1_fastqc.zip`); `None` when `name` is not one.
pub(crate) fn zip_beside_html(name: &str) -> Option<String> {
    stem_of(name, HTML_SUFFIX).map(|stem| format!("{stem}{ZIP_SUFFIX}"))
}

/// The FastQC HTML report beside a zip named `name` (`S1_fastqc.zip` → `S1_fastqc.html`); `None`
/// when `name` is not one.
pub(crate) fn html_beside_zip(name: &str) -> Option<String> {
    stem_of(name, ZIP_SUFFIX).map(|stem| format!("{stem}{HTML_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../../tests/fixtures/fastqc/fastqc_data.txt");

    fn module<'a>(data: &'a FastqcData, name: &str) -> &'a FastqcModule {
        data.modules.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name}"))
    }

    #[test]
    fn a_real_fastqc_0_12_1_file_parses_into_its_modules() {
        let data = parse(REAL);
        assert_eq!(data.version.as_deref(), Some("0.12.1"));
        assert!(!data.truncated);
        let names: Vec<&str> = data.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Basic Statistics",
                "Per base sequence quality",
                "Per sequence quality scores",
                "Per base sequence content",
                "Per sequence GC content",
                "Per base N content",
                "Sequence Length Distribution",
                "Sequence Duplication Levels",
                "Overrepresented sequences",
                "Adapter Content",
            ]
        );
        let basic = module(&data, "Basic Statistics");
        assert_eq!(basic.status, "pass");
        assert_eq!(basic.columns, ["Measure", "Value"]);
        assert!(basic.rows.contains(&vec!["Total Sequences".to_string(), "2000".to_string()]));
        assert!(basic.extra.is_empty());

        let quality = module(&data, "Per base sequence quality");
        assert_eq!(quality.status, "fail");
        assert_eq!(quality.columns[0], "Base");
        assert_eq!(quality.columns.len(), 7);
        assert!(quality.rows.iter().all(|r| r.len() == 7));
        // Grouped bases stay as written.
        assert!(quality.rows.iter().any(|r| r[0].contains('-')));

        let dup = module(&data, "Sequence Duplication Levels");
        assert_eq!(dup.extra, [["Total Deduplicated Percentage".to_string(), "91.8".to_string()]]);
        assert_eq!(dup.columns, ["Duplication Level", "Percentage of total"]);
        assert_eq!(module(&data, "Overrepresented sequences").status, "warn");
        assert!(data.modules.iter().all(|m| !m.truncated));
    }

    #[test]
    fn crlf_unterminated_modules_and_stray_lines_are_tolerated() {
        let text =
            "stray\r\n##FastQC\t0.11.9\r\n>>A\tWARN\r\n#x\ty\r\n1\t2\r\n\r\n>>END_MODULE\r\n\
                    noise outside\n>>B\tpass\n#only\n3\n>>C\tfail\n#h\n4\n";
        let data = parse(text);
        assert_eq!(data.version.as_deref(), Some("0.11.9"));
        assert_eq!(data.modules.len(), 3);
        assert_eq!(data.modules[0].status, "warn");
        assert_eq!(data.modules[0].columns, ["x", "y"]);
        assert_eq!(data.modules[0].rows, [vec!["1", "2"]]);
        // B is closed by C's start; C by the end of the file.
        assert_eq!(data.modules[1].rows, [vec!["3"]]);
        assert_eq!(data.modules[2].name, "C");
        assert_eq!(data.modules[2].rows, [vec!["4"]]);
        // A module line with no status, and no data at all.
        let bare = parse(">>Odd module\n>>END_MODULE\n");
        assert_eq!(bare.modules[0].status, "");
        assert!(bare.modules[0].columns.is_empty());
        assert!(parse("").modules.is_empty());
        assert_eq!(parse("").version, None);
    }

    #[test]
    fn a_long_module_keeps_its_first_rows_and_is_marked_truncated() {
        let mut text = String::from(">>Overrepresented sequences\twarn\n#Sequence\tCount\n");
        for i in 0..MAX_ROWS_PER_MODULE + 5 {
            text.push_str(&format!("S{i}\t{i}\n"));
        }
        text.push_str(">>END_MODULE\n");
        let m = &parse(&text).modules[0];
        assert_eq!(m.rows.len(), MAX_ROWS_PER_MODULE);
        assert_eq!(m.rows[0], ["S0", "0"]);
        assert!(m.truncated);
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn the_data_is_read_from_the_zip() {
        // The committed zip is FastQC's own (images and HTML dropped).
        let real = include_bytes!("../../tests/fixtures/fastqc/S1_R1_fastqc.zip");
        let data = from_zip(real).unwrap();
        assert_eq!(data, parse(REAL));

        let built = zip_of(&[
            ("S2_fastqc/summary.txt", b"PASS\tBasic Statistics\tS2.fq\n"),
            ("S2_fastqc/fastqc_data.txt", REAL.as_bytes()),
        ]);
        assert_eq!(from_zip(&built).unwrap().modules.len(), 10);
    }

    #[test]
    fn a_bad_zip_is_an_error_never_a_panic() {
        assert!(matches!(from_zip(b"not a zip"), Err(FastqcError::Zip(_))));
        assert!(matches!(from_zip(&[]), Err(FastqcError::Zip(_))));
        let real = include_bytes!("../../tests/fixtures/fastqc/S1_R1_fastqc.zip");
        for cut in [10, real.len() / 2, real.len() - 1] {
            assert!(from_zip(&real[..cut]).is_err(), "cut at {cut}");
        }
        let no_data = zip_of(&[("S2_fastqc/summary.txt", b"PASS\n")]);
        assert_eq!(from_zip(&no_data), Err(FastqcError::NoData));
        let empty = zip_of(&[("S2_fastqc/fastqc_data.txt", b"hello\n")]);
        assert_eq!(from_zip(&empty), Err(FastqcError::NoModules));
        // A bit flipped anywhere: an error or a parse, never a panic.
        for i in (0..real.len()).step_by(7) {
            let mut bent = real.to_vec();
            bent[i] ^= 0x5a;
            let _ = from_zip(&bent);
        }
    }

    #[test]
    fn a_decompression_bomb_is_refused() {
        let big = vec![b'x'; (MAX_DATA_BYTES + 10) as usize];
        let bomb = zip_of(&[("a_fastqc/fastqc_data.txt", &big)]);
        assert!(bomb.len() < 1 << 20, "deflated small");
        assert_eq!(from_zip(&bomb), Err(FastqcError::TooLarge));
    }

    #[test]
    fn names_follow_fastqc_s_own_rules() {
        for (input, zip) in [
            ("S1_R1.fastq.gz", "S1_R1_fastqc.zip"),
            ("S1.fq", "S1_fastqc.zip"),
            ("S1.fq.bz2", "S1_fastqc.zip"),
            ("reads.txt.gz", "reads_fastqc.zip"),
            ("x.bam", "x_fastqc.zip"),
            ("x.FASTQ.GZ", "x.FASTQ.GZ_fastqc.zip"),
            ("plain", "plain_fastqc.zip"),
        ] {
            assert_eq!(zip_name_for(input), zip, "{input}");
        }
        assert_eq!(zip_beside_html("S1_fastqc.html").as_deref(), Some("S1_fastqc.zip"));
        assert_eq!(zip_beside_html("S1.html"), None);
        assert_eq!(zip_beside_html("é"), None);
        assert_eq!(html_beside_zip("S1_fastqc.zip").as_deref(), Some("S1_fastqc.html"));
        assert_eq!(html_beside_zip("S1_FASTQC.ZIP").as_deref(), Some("S1_fastqc.html"));
        // A bare suffix has no stem: not FastQC's.
        assert_eq!(html_beside_zip("_fastqc.zip"), None);
        assert_eq!(zip_beside_html("_fastqc.html"), None);
        assert!(is_zip("a_FastQC.zip"));
        assert!(!is_zip("fastqc.zip"));
        assert!(!is_zip("_fastqc.zip"));
        assert!(!is_zip("a_fastqc.html"));
        for fastq in ["S1.fastq.gz", "S1.fq", "S1.FQ.GZ", "x.fastq.bz2", "r.csfastq"] {
            assert!(is_fastq(fastq), "{fastq}");
        }
        for other in ["S1.bam", "S1.fastq.zip", ".fq", "fq", "a.txt.gz"] {
            assert!(!is_fastq(other), "{other}");
        }
    }

    #[test]
    fn a_hash_line_after_the_data_is_an_extra() {
        let m = &parse(">>M\tpass\n#k\tv\n#h\n1\n#late\tx\n>>END_MODULE\n").modules[0];
        assert_eq!(m.columns, ["h"]);
        assert_eq!(
            m.extra,
            [["k".to_string(), "v".to_string()], ["late".to_string(), "x".to_string()]]
        );
        assert_eq!(m.rows, [vec!["1"]]);
    }

    #[test]
    fn what_a_file_can_make_the_parser_keep_is_bounded() {
        // A row of 100,000 cells keeps MAX_COLUMNS cells and marks its module.
        let wide = format!(">>M\tpass\n#h\n{}\n>>END_MODULE\n", "x\t".repeat(100_000));
        let data = parse(&wide);
        assert_eq!(data.modules[0].rows[0].len(), MAX_COLUMNS);
        assert!(data.modules[0].truncated);
        // So does a header that wide.
        let header = format!(">>M\tpass\n#{}\n1\n", "c\t".repeat(1000));
        assert_eq!(parse(&header).modules[0].columns.len(), MAX_COLUMNS);
        // 10,000 modules: the first MAX_MODULES are kept.
        let many = ">>m\tpass\n".repeat(10_000);
        let data = parse(&many);
        assert_eq!(data.modules.len(), MAX_MODULES);
        assert!(data.truncated);
        // `#` lines past the cap are dropped.
        let hashes = format!(">>M\tpass\n{}1\n", "#k\tv\n".repeat(1000));
        let m = &parse(&hashes).modules[0];
        assert_eq!(m.extra.len() + 1, MAX_HASH_LINES);
        assert!(m.truncated);
        // The cells in all: rows stop at MAX_CELLS, wherever they are.
        let row = format!("{}\n", ["1"; MAX_COLUMNS].join("\t"));
        let mut text = String::new();
        for i in 0..MAX_MODULES {
            text.push_str(&format!(">>M{i}\tpass\n#h\n"));
            text.push_str(&row.repeat(MAX_ROWS_PER_MODULE));
        }
        let data = parse(&text);
        let cells: usize = data.modules.iter().flat_map(|m| m.rows.iter().map(Vec::len)).sum();
        assert!(cells <= MAX_CELLS, "{cells}");
        assert!(data.truncated);
        // And the bytes they hold.
        let fat = format!(">>M\tpass\n#h\n{}\n", "x".repeat(MAX_CELL_BYTES + 1));
        let data = parse(&fat);
        assert!(data.modules[0].rows.is_empty());
        assert!(data.modules[0].truncated && data.truncated);
    }

    #[test]
    fn a_zip64_archive_is_refused_before_the_zip_reader_sizes_anything() {
        let refused = |bytes: &[u8]| matches!(from_zip(bytes), Err(FastqcError::Zip(m)) if m.contains("zip64"));
        // A zip64 locator right before the end record, which itself carries no marker.
        let mut located = vec![0u8; 1 << 16];
        located.extend(b"PK\x06\x07");
        located.extend([0u8; 16]);
        located.extend(b"PK\x05\x06");
        located.extend([0u8; 18]);
        assert!(refused(&located));
        // An end record whose entry count is the zip64 marker, with no locator.
        let mut marked = vec![0u8; 1 << 16];
        marked.extend(b"PK\x05\x06");
        marked.extend([0u8; 6]);
        marked.extend([0xffu8; 4]);
        marked.extend([0u8; 10]);
        assert!(refused(&marked));
        // Either, with a decoy end record after it.
        for mut crafted in [located, marked] {
            crafted.extend(b"PK\x05\x06");
            crafted.extend([0u8; 18]);
            assert!(refused(&crafted));
        }
    }

    #[test]
    fn zip64_signatures_in_an_entry_s_data_are_not_a_zip64_archive() {
        // An image whose bytes happen to hold both zip64 signatures, stored as they are.
        let mut png = b"\x89PNG....".to_vec();
        png.extend(b"PK\x06\x06 and PK\x06\x07");
        let zip = {
            use std::io::Write;
            let stored = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            w.start_file("S_fastqc/Images/per_base_quality.png", stored).unwrap();
            w.write_all(&png).unwrap();
            w.start_file("S_fastqc/fastqc_data.txt", stored).unwrap();
            w.write_all(REAL.as_bytes()).unwrap();
            w.finish().unwrap().into_inner()
        };
        assert!(zip.windows(4).any(|w| w == b"PK\x06\x06"), "stored as it is");
        assert_eq!(from_zip(&zip).unwrap().modules.len(), 10);
    }

    #[test]
    fn the_shallowest_fastqc_data_is_read_whichever_comes_first() {
        let junk = b"##FastQC\t0.12.1\n>>Junk\tpass\n>>END_MODULE\n".as_slice();
        for files in [
            [("a/b/fastqc_data.txt", junk), ("S_fastqc/fastqc_data.txt", REAL.as_bytes())],
            [("S_fastqc/fastqc_data.txt", REAL.as_bytes()), ("a/b/fastqc_data.txt", junk)],
        ] {
            assert_eq!(from_zip(&zip_of(&files)).unwrap(), parse(REAL));
        }
    }
}
