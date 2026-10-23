//! Reading a workflow's source without running it: the container images its tasks name, for
//! `biffix inspect` to add to the `images:` block of `biffix.yaml` (which is all registration
//! imports), and (in [`resources`]) the Snakemake rules that would run with Snakemake's default
//! memory.
//!
//! - **Nextflow**: the engine's own `nextflow inspect -format json`, which resolves each process's
//!   `container` (ternaries, params, configs, `docker.registry`) — [`from_nextflow_inspect`].
//! - **Snakemake**: `aws_batch_container_image` (the AWS Batch executor's per-rule image) wherever
//!   the source sets it — rule `resources:`, a helper module's resources dict, a profile or config
//!   file — [`scan_snakemake`]. A plain string is an image; anything else (a function, a config
//!   lookup, a string that an expression continues) is listed for the author to declare by hand,
//!   and an unset value (`null`, `None`) is skipped. `container_image`, where it is set as a rule
//!   resource, is reported as a key the executor ignores. Nothing is executed.
//!
//! Either way, a value that does not parse as an image reference is listed for the author too.
//!
//! Tasks resolve images strictly at run time, so an image missed here fails its task with a message
//! naming it rather than pulling from a public registry.

pub mod resources;

use serde::{Deserialize, Serialize};

/// What one occurrence is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind {
    /// An image reference.
    Literal,
    /// A value that is not a plain string (a function, a lookup): declare its images by hand.
    Dynamic,
    /// A key the engine does not read for the task image (Snakemake `container_image`).
    IgnoredKey,
}

/// One place a workflow names (or may name) a task image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanEntry {
    /// Where: `rules/align.smk:12`, or `process FASTQC`.
    pub location: String,
    /// The key (`aws_batch_container_image`, `container`, …).
    pub key: String,
    pub kind: ScanKind,
    /// The image reference, or the expression's text.
    pub value: String,
}

impl ScanEntry {
    /// An occurrence, for either engine: a value taken as `Literal` that is not an image
    /// reference ([`crate::images::ImageRef::parse`]: a `docker://` or Singularity URL, an
    /// uppercase name, a `{placeholder}`) is `Dynamic`, so the author sees it under "declare by
    /// hand" instead of it being dropped as unparseable.
    fn new(location: String, key: &str, kind: ScanKind, value: String) -> Self {
        let kind = if kind == ScanKind::Literal && crate::images::ImageRef::parse(&value).is_err() {
            ScanKind::Dynamic
        } else {
            kind
        };
        Self { location, key: key.into(), kind, value }
    }
}

/// What an inspection found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageScan {
    pub entries: Vec<ScanEntry>,
}

impl ImageScan {
    /// The image references found, deduplicated, each with where it was first seen.
    pub fn references(&self) -> Vec<(&str, &str)> {
        let mut seen = std::collections::BTreeSet::new();
        self.entries
            .iter()
            .filter(|e| e.kind == ScanKind::Literal && seen.insert(e.value.as_str()))
            .map(|e| (e.value.as_str(), e.location.as_str()))
            .collect()
    }

    /// Everything that is not an image reference (to declare by hand, or a key to fix).
    pub fn unresolved(&self) -> impl Iterator<Item = &ScanEntry> {
        self.entries.iter().filter(|e| e.kind != ScanKind::Literal)
    }
}

/// Why `nextflow inspect` output could not be read.
#[derive(Debug, thiserror::Error)]
#[error("not `nextflow inspect -format json` output: {0}")]
pub struct InspectError(String);

/// The images in `nextflow inspect -format json` output
/// (`{"processes": [{"name": …, "container": …}]}`).
pub fn from_nextflow_inspect(json: &str) -> Result<ImageScan, InspectError> {
    #[derive(Deserialize)]
    struct Output {
        processes: Vec<Process>,
    }
    #[derive(Deserialize)]
    struct Process {
        name: String,
        #[serde(default)]
        container: Option<String>,
    }
    let output: Output = serde_json::from_str(json).map_err(|e| InspectError(e.to_string()))?;
    let entries = output
        .processes
        .into_iter()
        .filter_map(|p| {
            let container = p.container?.trim().to_string();
            (!container.is_empty()).then(|| {
                ScanEntry::new(
                    format!("process {}", p.name),
                    "container",
                    ScanKind::Literal,
                    container,
                )
            })
        })
        .collect();
    Ok(ImageScan { entries })
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 1-based line of byte offset `at`.
fn line_of(text: &str, at: usize) -> usize {
    text.as_bytes()[..at].iter().filter(|b| **b == b'\n').count() + 1
}

/// Positions of `word` in `text` that stand alone (no identifier character on either side).
fn word_positions(text: &str, word: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    text.match_indices(word)
        .map(|(at, _)| at)
        .filter(|at| {
            let before = at.checked_sub(1).map(|i| bytes[i]);
            let after = bytes.get(at + word.len()).copied();
            !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
        })
        .collect()
}

/// A one-line quoted string at `at` (`'…'` or `"…"`): its body, and whether it interpolates (`{`
/// in an f-string).
fn quoted_at(text: &str, at: usize, fstring: bool) -> Option<(String, bool)> {
    let quote = *text.as_bytes().get(at)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let rest = &text[at + 1..];
    let end = rest.find([quote as char, '\n'])?;
    if rest.as_bytes()[end] == b'\n' || rest[..end].contains('\\') {
        return None;
    }
    let body = rest[..end].to_string();
    let interpolates = fstring && body.contains('{');
    Some((body, interpolates))
}

/// The value's text from `at`: the rest of the line, ended where its syntax ends rather than at the
/// line's end. It stops before a comment (a `#` outside quotes that begins the text or follows a
/// space or tab), at a `,` or a closing `)`/`]`/`}` outside quotes and outside brackets the value
/// opened (the next item of a flow collection or argument list, or the collection's end), and at
/// `close`: the quote a quoted list item opened before its key (`- "key=value"`).
fn value_text(text: &str, at: usize, close: Option<u8>) -> String {
    let end = text[at..].find('\n').map_or(text.len(), |n| at + n);
    let line = &text[at..end];
    let mut quote = None;
    let mut depth = 0usize;
    let mut cut = line.len();
    for (i, b) in line.bytes().enumerate() {
        match (quote, b) {
            (Some(q), c) => {
                if c == q {
                    quote = None;
                }
            }
            (None, c) if Some(c) == close => {
                cut = i;
                break;
            }
            (None, b'#') if i == 0 || matches!(line.as_bytes()[i - 1], b' ' | b'\t') => {
                cut = i;
                break;
            }
            (None, b'\'' | b'"') => quote = Some(b),
            (None, b'(' | b'[' | b'{') => depth += 1,
            (None, b',' | b')' | b']' | b'}') if depth == 0 => {
                cut = i;
                break;
            }
            (None, b')' | b']' | b'}') => depth -= 1,
            _ => {}
        }
    }
    line[..cut].trim().to_string()
}

/// Whether the text from `at` (just after a closing quote, or a word) ends the value: a separator
/// (`,`, `)`, `}`, `]`), a comment or the end of the line (`\r\n` included). Anything else
/// (`+ tag`, `if x else "b"`, `.format(…)`) continues an expression, so the string is not the
/// value.
fn string_ends_value(text: &str, at: usize) -> bool {
    matches!(
        text.as_bytes().get(skip_ws(text, at)),
        None | Some(b',' | b')' | b'}' | b']' | b'#' | b'\r' | b'\n')
    )
}

/// The YAML plain scalars PyYAML (what Snakemake reads profiles and configs with) resolves to null
/// (JSON's `null` among them): the resource is unset, so there is no image to declare.
const YAML_NULLS: [&str; 4] = ["~", "null", "Null", "NULL"];

/// A YAML plain scalar that PyYAML resolves to a bool or a number rather than a string.
fn is_yaml_non_string(s: &str) -> bool {
    static NON_STRING: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // PyYAML's (YAML 1.1) bool, int and float resolvers.
    let re = NON_STRING.get_or_init(|| {
        regex::Regex::new(
            r"^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF|[-+]?(?:0b[01_]+|0[0-7_]+|0|[1-9][0-9_]*|0x[0-9a-fA-F_]+|[1-9][0-9_]*(?::[0-5]?[0-9])+)|[-+]?(?:[0-9][0-9_]*\.[0-9_]*(?:[eE][-+][0-9]+)?|\.[0-9_]+(?:[eE][-+][0-9]+)?|[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*|\.(?:inf|Inf|INF))|\.(?:nan|NaN|NAN))$",
        )
        .expect("a valid pattern")
    });
    re.is_match(s)
}

/// Whether offset `at` sits in a `#` comment: a `#` outside quotes earlier on its line.
fn in_hash_comment(text: &str, at: usize) -> bool {
    let start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let mut quote = None;
    for b in text[start..at].bytes() {
        match (quote, b) {
            (None, b'#') => return true,
            (None, b'\'' | b'"') => quote = Some(b),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
    }
    false
}

/// Whether the unquoted Python key at `at` is a keyword argument or a `resources:` item: the code
/// before it (`masked`: comments and long strings blanked) ends in `(`, `,` or `{`, or in the `:`
/// of a `resources:` directive. A statement (`key = …` after other code) is neither.
fn is_resource_item(masked: &str, at: usize) -> bool {
    let before = masked[..at].trim_end();
    match before.as_bytes().last() {
        Some(b'(' | b',' | b'{') => true,
        Some(b':') => {
            let directive = before[..before.len() - 1].trim_end();
            directive
                .strip_suffix("resources")
                .is_some_and(|rest| !rest.bytes().last().is_some_and(is_ident))
        }
        _ => false,
    }
}

/// Whether the YAML/JSON key at `at` sits under a `set-resources` or `default-resources` mapping:
/// on its own line (a flow collection), or as an ancestor by indentation.
fn under_resources(text: &str, at: usize) -> bool {
    const KEYS: [&str; 2] = ["set-resources", "default-resources"];
    let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    if KEYS.iter().any(|k| text[line_start..at].contains(k)) {
        return true;
    }
    let indent_of = |line: &str| line.len() - line.trim_start().len();
    let mut indent = indent_of(&text[line_start..]);
    for line in text[..line_start].lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || indent_of(line) >= indent {
            continue;
        }
        indent = indent_of(line);
        let key = trimmed.trim_start_matches("- ").trim_start_matches(['"', '\'']);
        if KEYS.iter().any(|k| key.starts_with(k)) {
            return true;
        }
        if indent == 0 {
            break;
        }
    }
    false
}

fn skip_ws(text: &str, mut at: usize) -> usize {
    let bytes = text.as_bytes();
    while at < bytes.len() && (bytes[at] == b' ' || bytes[at] == b'\t') {
        at += 1;
    }
    at
}

/// A Python-like source (a Snakefile, a rule file, a helper module) with what is not code blanked,
/// and where each line stands.
pub(crate) struct Lexed {
    /// The text with every comment and every triple-quoted string body blanked to spaces (and,
    /// when asked, every one-line string body too). Newlines and the quote characters are kept, so
    /// offsets, line numbers and indentation are unchanged.
    pub masked: String,
    /// Per line (0-based), whether it continues the line before: it starts inside a string, inside
    /// an open bracket, or after a backslash. Its indentation means nothing to Python.
    pub continued: Vec<bool>,
}

/// Lex `text` as Python: see [`Lexed`]. `blank_short_strings` blanks one-line string bodies too.
/// Escapes are honoured; string prefixes (`r`, `f`, `b`) are left as they are.
pub(crate) fn lex_python(text: &str, blank_short_strings: bool) -> Lexed {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut continued = vec![false];
    let mut depth = 0usize;
    let mut backslash = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                continued.push(depth > 0 || backslash);
                backslash = false;
                i += 1;
            }
            b'\\' if b.get(i + 1) == Some(&b'\n') => {
                backslash = true;
                i += 1;
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            q @ (b'\'' | b'"') => {
                let triple = b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q);
                let open = if triple { 3 } else { 1 };
                let blank = triple || blank_short_strings;
                let mut j = i + open;
                while j < b.len() {
                    let c = b[j];
                    if c == b'\\' {
                        // An escaped newline continues the string onto the next line.
                        if b.get(j + 1) == Some(&b'\n') {
                            continued.push(true);
                        }
                        j += 2;
                        continue;
                    }
                    if c == b'\n' {
                        if !triple {
                            break;
                        }
                        continued.push(true);
                    }
                    if c == q && (!triple || (b.get(j + 1) == Some(&q) && b.get(j + 2) == Some(&q)))
                    {
                        break;
                    }
                    j += 1;
                }
                let end = j.min(b.len());
                if blank {
                    for k in (i + open).min(end)..end {
                        if b[k] != b'\n' {
                            out[k] = b' ';
                        }
                    }
                }
                i = if end < b.len() && b[end] == q { end + open } else { end };
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    Lexed {
        // Only whole characters between ASCII delimiters were replaced, with ASCII.
        masked: String::from_utf8(out).expect("masking keeps UTF-8"),
        continued,
    }
}

/// The files a Snakemake workflow can set a rule's resources in: a Snakefile (`Snakefile` or
/// `snakefile`, Snakemake's default names), an included rule file (`.smk`, `.snakefile`,
/// `.rules`, `.snake`), a Python helper, and a profile or config file.
fn is_snakemake_source(path: &str) -> bool {
    let name = crate::text::basename(path);
    name.eq_ignore_ascii_case("snakefile")
        || [".smk", ".snakefile", ".rules", ".snake", ".py", ".yaml", ".yml", ".json"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

/// Scan a Snakemake workflow's files (`(path, bytes)`, paths relative to the source root) for
/// `aws_batch_container_image` — `key=value` in Python/Snakefiles, `key: value` in YAML/JSON and
/// dict literals — and the ignored `container_image`.
pub fn scan_snakemake(files: &[(String, Vec<u8>)]) -> ImageScan {
    let mut entries = Vec::new();
    for (path, bytes) in files.iter().filter(|(p, _)| is_snakemake_source(p)) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        // Both keys contain `container_image`: a file without it has no entry, so skip its lexing.
        if !text.contains("container_image") {
            continue;
        }
        let yaml = path.ends_with(".yaml") || path.ends_with(".yml");
        let json = path.ends_with(".json");
        let python = !yaml && !json;
        // In Python and Snakefiles, a comment or a triple-quoted string (a `shell: """…"""` block,
        // a docstring) is blanked here; one-line strings are kept, as a quoted key is code.
        let masked = python.then(|| lex_python(text, false).masked);
        // The same with one-line strings blanked too: where a key sits inside a string.
        let in_string = python.then(|| lex_python(text, true).masked);
        for (key, ignored) in [("aws_batch_container_image", false), ("container_image", true)] {
            for at in word_positions(text, key) {
                let hidden = match &masked {
                    Some(masked) => masked.as_bytes()[at] != text.as_bytes()[at],
                    None => in_hash_comment(text, at),
                };
                if hidden {
                    continue;
                }
                // In Python a key inside a one-line string is code only as the whole string (a
                // dict key, a subscript); a word in a longer string (`shell: "echo key=…"`) is not.
                let quoted = at > 0
                    && matches!(text.as_bytes()[at - 1], b'"' | b'\'')
                    && text.as_bytes().get(at + key.len()) == Some(&text.as_bytes()[at - 1]);
                if let Some(in_string) = &in_string {
                    if in_string.as_bytes()[at] != text.as_bytes()[at] && !quoted {
                        continue;
                    }
                }
                // A quoted key (`"aws_batch_container_image": …`) closes its quote first, and a
                // subscript (`res["aws_batch_container_image"] = …`) its bracket.
                let mut sep = at + key.len();
                if matches!(text.as_bytes().get(sep), Some(b'"' | b'\'')) {
                    sep += 1;
                    if text.as_bytes().get(sep) == Some(&b']') {
                        sep += 1;
                    }
                }
                let sep = skip_ws(text, sep);
                let is_sep = match text.as_bytes().get(sep) {
                    Some(b'=') => text.as_bytes().get(sep + 1) != Some(&b'='),
                    // An unquoted Python `key:` is an annotation (`def f(key: str)`), no resource.
                    Some(b':') => !python || quoted,
                    _ => false,
                };
                if !is_sep {
                    continue;
                }
                // The ignored key is reported only where it is a rule resource: in Python a
                // keyword argument or a `resources:` item (not a variable of that name), in a
                // profile under `set-resources`/`default-resources` (not a workflow config key).
                // `aws_batch_container_image` is read anywhere: a variable or config value of that
                // name is how a workflow names one image for several rules.
                if ignored {
                    let resource = match &masked {
                        Some(masked) => quoted || is_resource_item(masked, at),
                        None => under_resources(text, at),
                    };
                    if !resource {
                        continue;
                    }
                }
                let mut value_at = skip_ws(text, sep + 1);
                let bytes = text.as_bytes();
                let prefixed = matches!(bytes.get(value_at), Some(b'f' | b'F' | b'r' | b'R'))
                    && matches!(bytes.get(value_at + 1), Some(b'\'' | b'"'));
                let fstring = prefixed && matches!(bytes[value_at], b'f' | b'F');
                if prefixed {
                    value_at += 1;
                }
                // A key that a quote opens without closing (`- "key=value"`) is inside a quoted
                // list item, which ends at that quote.
                let close = at
                    .checked_sub(1)
                    .map(|i| text.as_bytes()[i])
                    .filter(|q| matches!(q, b'"' | b'\''))
                    .filter(|q| text.as_bytes().get(at + key.len()) != Some(q));
                let rest = value_text(text, value_at, close);
                // An unset value (YAML/JSON `null`, Python `None`, and in YAML nothing at all)
                // names no image.
                let nulls: &[&str] = if python { &["None"] } else { &YAML_NULLS };
                let unset = (yaml && rest.is_empty())
                    || nulls.iter().any(|null| {
                        text[value_at..].starts_with(null)
                            && string_ends_value(text, value_at + null.len())
                    });
                if unset {
                    continue;
                }
                let (kind, value) = if ignored {
                    (ScanKind::IgnoredKey, rest)
                } else {
                    match quoted_at(text, value_at, fstring) {
                        // The whole value only when nothing continues the expression after it.
                        Some((literal, false))
                            if string_ends_value(text, value_at + literal.len() + 2) =>
                        {
                            (ScanKind::Literal, literal.trim().to_string())
                        }
                        // A YAML plain scalar is a string too, unless it is a bool or a number.
                        None if yaml
                            && !rest.is_empty()
                            && !rest.starts_with(['{', '[', '&', '*', '!', '|', '>'])
                            && !is_yaml_non_string(&rest) =>
                        {
                            (ScanKind::Literal, rest)
                        }
                        _ => (ScanKind::Dynamic, rest),
                    }
                };
                let location = format!("{path}:{}", line_of(text, at));
                entries.push((path.clone(), at, ScanEntry::new(location, key, kind, value)));
            }
        }
    }
    entries.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    ImageScan { entries: entries.into_iter().map(|(_, _, e)| e).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        list.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect()
    }

    fn found(scan: &ImageScan) -> Vec<(&str, ScanKind, &str)> {
        scan.entries.iter().map(|e| (e.location.as_str(), e.kind, e.value.as_str())).collect()
    }

    #[test]
    fn snakemake_images_are_found_wherever_resources_are_set() {
        let snakefile = r#"include: "rules/align.smk"

rule fastqc:
    resources:
        mem_mb=1024,
        aws_batch_container_image="quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0",
    shell: "fastqc"

rule inline:
    resources: aws_batch_container_image='ubuntu:24.04', mem_mb=10
"#;
        let align = "rule align:\r\n    resources:\r\n        aws_batch_container_image = \"quay.io/b/bwa:1\"\r\n";
        let helper = "GPU = {\"aws_batch_container_image\": \"quay.io/b/gpu:1\", \"mem_mb\": 1}\n";
        let profile =
            "set-resources:\n  multiqc:\n    aws_batch_container_image: quay.io/b/multiqc:1.25  # pinned\n";
        let scan = scan_snakemake(&files(&[
            ("workflow/Snakefile", snakefile),
            ("workflow/rules/align.smk", align),
            ("workflow/lib/resources.py", helper),
            ("profiles/default/config.yaml", profile),
            ("README.md", "aws_batch_container_image=\"nope:1\""),
        ]));
        assert_eq!(
            found(&scan),
            [
                ("profiles/default/config.yaml:3", ScanKind::Literal, "quay.io/b/multiqc:1.25"),
                (
                    "workflow/Snakefile:6",
                    ScanKind::Literal,
                    "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0"
                ),
                ("workflow/Snakefile:10", ScanKind::Literal, "ubuntu:24.04"),
                ("workflow/lib/resources.py:1", ScanKind::Literal, "quay.io/b/gpu:1"),
                ("workflow/rules/align.smk:3", ScanKind::Literal, "quay.io/b/bwa:1"),
            ]
        );
        assert_eq!(scan.references().len(), 5);
    }

    #[test]
    fn snakemake_values_that_are_not_strings_are_listed_for_the_author() {
        let smk = r#"rule a:
    resources:
        aws_batch_container_image=lambda wc: image_for_arch(wc.arch),
        # aws_batch_container_image="commented:1",
rule b:
    resources:
        aws_batch_container_image=f"quay.io/x/{config['tool']}:1",
        container_image = lambda wc: image_for_arch(wc.arch),
    run:
        if x.aws_batch_container_image == "y": pass
"#;
        let scan = scan_snakemake(&files(&[("rules/a.smk", smk)]));
        assert_eq!(
            found(&scan),
            [
                ("rules/a.smk:3", ScanKind::Dynamic, "lambda wc: image_for_arch(wc.arch)"),
                ("rules/a.smk:7", ScanKind::Dynamic, "\"quay.io/x/{config['tool']}:1\""),
                ("rules/a.smk:8", ScanKind::IgnoredKey, "lambda wc: image_for_arch(wc.arch)"),
            ]
        );
        assert!(scan.references().is_empty());
        assert_eq!(scan.unresolved().count(), 3);
    }

    #[test]
    fn a_string_an_expression_continues_is_not_the_value() {
        let smk = r#"rule a:
    resources:
        aws_batch_container_image="quay.io/b/x:1" if config["gpu"] else "quay.io/b/y:1",
rule b:
    resources:
        aws_batch_container_image="quay.io/b/x:" + config["tag"],
rule c:
    resources:
        aws_batch_container_image="quay.io/b/{}:1".format(config["tool"]),
rule d:
    resources:
        aws_batch_container_image=r"quay.io/b/raw:1"  # a raw string is a plain string
rule e:
    resources:
        aws_batch_container_image=("quay.io/b/paren:1")
rule f:
    resources:
        aws_batch_container_image="{IMAGE}"
"#;
        let scan = scan_snakemake(&files(&[("rules/a.smk", smk)]));
        assert_eq!(
            found(&scan),
            [
                (
                    "rules/a.smk:3",
                    ScanKind::Dynamic,
                    r#""quay.io/b/x:1" if config["gpu"] else "quay.io/b/y:1""#
                ),
                ("rules/a.smk:6", ScanKind::Dynamic, r#""quay.io/b/x:" + config["tag"]"#),
                ("rules/a.smk:9", ScanKind::Dynamic, r#""quay.io/b/{}:1".format(config["tool"])"#),
                ("rules/a.smk:12", ScanKind::Literal, "quay.io/b/raw:1"),
                ("rules/a.smk:15", ScanKind::Dynamic, r#"("quay.io/b/paren:1")"#),
                // A plain string that is not an image reference.
                ("rules/a.smk:18", ScanKind::Dynamic, "{IMAGE}"),
            ]
        );
    }

    #[test]
    fn unset_and_non_string_values_are_not_images() {
        let profile = "set-resources:\n  z:\n    aws_batch_container_image:\n  a:\n    \
                       aws_batch_container_image: null\n  b:\n    \
                       aws_batch_container_image: ~\n  c:\n    aws_batch_container_image: true\n  \
                       d:\n    aws_batch_container_image: 42\n  e:\n    \
                       aws_batch_container_image: 1.5\n  f:\n    \
                       aws_batch_container_image: quay.io/b/f:1\t# tab before the comment\n";
        let helper = "RES = dict(aws_batch_container_image=None)\n";
        let json = "{\"aws_batch_container_image\": null}\n";
        let scan = scan_snakemake(&files(&[
            ("profiles/p/config.yaml", profile),
            ("lib/res.py", helper),
            ("config.json", json),
        ]));
        assert_eq!(
            found(&scan),
            [
                // `z`'s empty value is null in YAML: no entry.
                ("profiles/p/config.yaml:9", ScanKind::Dynamic, "true"),
                ("profiles/p/config.yaml:11", ScanKind::Dynamic, "42"),
                ("profiles/p/config.yaml:13", ScanKind::Dynamic, "1.5"),
                ("profiles/p/config.yaml:15", ScanKind::Literal, "quay.io/b/f:1"),
            ]
        );
        // A YAML scalar that is only number-like is still a string (and an image).
        assert!(!is_yaml_non_string("ubuntu"));
        assert!(!is_yaml_non_string("1.25-alpine"));
        assert!(is_yaml_non_string("0x1F") && is_yaml_non_string("-.inf"));
    }

    #[test]
    fn a_value_ends_where_its_syntax_ends_not_at_the_end_of_the_line() {
        // Flow collections and a quoted list item close after the value.
        let flow = "set-resources: {m: {aws_batch_container_image: quay.io/b/m:1}}\n\
                    default-resources: [aws_batch_container_image=quay.io/b/x:1]\n";
        let listed = "default-resources:\n  - \"aws_batch_container_image=quay.io/b/y:1\"\n  \
                      - 'aws_batch_container_image=quay.io/b/z:1'\n";
        // A sibling resource is no part of an expression's text.
        let smk = "rule a:\n    resources: aws_batch_container_image=IMG, mem_mb=10\n";
        let scan = scan_snakemake(&files(&[
            ("profiles/a/config.yaml", flow),
            ("profiles/b/config.yaml", listed),
            ("rules/a.smk", smk),
        ]));
        assert_eq!(
            found(&scan),
            [
                ("profiles/a/config.yaml:1", ScanKind::Literal, "quay.io/b/m:1"),
                ("profiles/a/config.yaml:2", ScanKind::Literal, "quay.io/b/x:1"),
                ("profiles/b/config.yaml:2", ScanKind::Literal, "quay.io/b/y:1"),
                ("profiles/b/config.yaml:3", ScanKind::Literal, "quay.io/b/z:1"),
                ("rules/a.smk:2", ScanKind::Dynamic, "IMG"),
            ]
        );
    }

    #[test]
    fn container_image_is_reported_only_where_it_is_a_resource_key() {
        // A variable, a parameter's annotation, a workflow config key and a word in a shell
        // string are no rule resource, so "the task runs the default image" is not said of them.
        let smk = "container_image = \"quay.io/b/v:1\"\n\
                   \n\
                   def f(container_image: str, aws_batch_container_image: str):\n    \
                       pass\n\
                   \n\
                   rule s:\n    \
                       shell: \"echo aws_batch_container_image=quay.io/b/sh:1\"\n\
                   rule a:\n    \
                       resources:\n        \
                           mem_mb=1,\n        \
                           container_image=\"quay.io/b/c:1\",\n\
                   rule b:\n    \
                       resources: container_image=\"quay.io/b/d:1\"\n";
        let config = "container_image: quay.io/b/conf:1\n";
        let profile = "set-resources:\n  a:\n    container_image: quay.io/b/p:1\n";
        let scan = scan_snakemake(&files(&[
            ("workflow/Snakefile", smk),
            ("config/config.yaml", config),
            ("profiles/p/config.yaml", profile),
        ]));
        assert_eq!(
            found(&scan),
            [
                ("profiles/p/config.yaml:3", ScanKind::IgnoredKey, "quay.io/b/p:1"),
                ("workflow/Snakefile:11", ScanKind::IgnoredKey, "\"quay.io/b/c:1\""),
                ("workflow/Snakefile:13", ScanKind::IgnoredKey, "\"quay.io/b/d:1\""),
            ]
        );
    }

    #[test]
    fn every_snakemake_file_kind_is_read_and_a_subscript_sets_the_image() {
        let line = "aws_batch_container_image=\"quay.io/b/x:1\"\n";
        let subscript = "res[\"aws_batch_container_image\"] = \"quay.io/b/sub:1\"\n\
                         res['aws_batch_container_image'] = image_for(wc)\n\
                         if res[\"aws_batch_container_image\"] == \"y\": pass\n";
        let scan = scan_snakemake(&files(&[
            ("workflow/snakefile", line),
            ("rules/a.rules", line),
            ("rules/b.snake", line),
            ("lib/res.py", subscript),
        ]));
        assert_eq!(
            found(&scan),
            [
                ("lib/res.py:1", ScanKind::Literal, "quay.io/b/sub:1"),
                ("lib/res.py:2", ScanKind::Dynamic, "image_for(wc)"),
                ("rules/a.rules:1", ScanKind::Literal, "quay.io/b/x:1"),
                ("rules/b.snake:1", ScanKind::Literal, "quay.io/b/x:1"),
                ("workflow/snakefile:1", ScanKind::Literal, "quay.io/b/x:1"),
            ]
        );
    }

    #[test]
    fn an_image_inside_a_triple_quoted_string_is_not_read() {
        let smk = "rule a:\n    shell:\n        \"\"\"\n        echo aws_batch_container_image=\"quay.io/b/x:1\"\n        \"\"\"\n    resources:\n        aws_batch_container_image=\"quay.io/b/real:1\"  # aws_batch_container_image=\"c:1\"\n";
        let scan = scan_snakemake(&files(&[("rules/a.smk", smk)]));
        assert_eq!(found(&scan), [("rules/a.smk:7", ScanKind::Literal, "quay.io/b/real:1")]);
    }

    #[test]
    fn nextflow_inspect_output_lists_each_process_container() {
        // The shape `nextflow inspect -format json` (25.10) prints.
        let json = r#"{"processes": [
            {"name": "FASTQC", "container": "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0"},
            {"name": "MULTIQC", "container": "quay.io/biocontainers/multiqc:1.25--pyhdfd78af_0"},
            {"name": "FASTQC_AGAIN", "container": "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0"},
            {"name": "LOCAL", "container": null}
        ]}"#;
        let scan = from_nextflow_inspect(json).unwrap();
        assert_eq!(
            scan.references(),
            [
                ("quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0", "process FASTQC"),
                ("quay.io/biocontainers/multiqc:1.25--pyhdfd78af_0", "process MULTIQC"),
            ]
        );
        // A container that is not an image reference is listed for the author, not dropped.
        let json = r#"{"processes": [
            {"name": "SING", "container": "https://depot.galaxyproject.org/singularity/fastqc:0.12.1--hdfd78af_0"},
            {"name": "DOCKER", "container": "docker://quay.io/b/x:1"},
            {"name": "UPPER", "container": "quay.io/Biocontainers/X:1"},
            {"name": "OK", "container": "quay.io/b/y:1"}
        ]}"#;
        let scan = from_nextflow_inspect(json).unwrap();
        assert_eq!(scan.references(), [("quay.io/b/y:1", "process OK")]);
        let unresolved: Vec<(&str, ScanKind)> =
            scan.unresolved().map(|e| (e.location.as_str(), e.kind)).collect();
        assert_eq!(
            unresolved,
            [
                ("process SING", ScanKind::Dynamic),
                ("process DOCKER", ScanKind::Dynamic),
                ("process UPPER", ScanKind::Dynamic),
            ]
        );
        assert!(from_nextflow_inspect("[]").is_err());
        assert!(from_nextflow_inspect("{\"processes\": []}").unwrap().entries.is_empty());
    }
}
