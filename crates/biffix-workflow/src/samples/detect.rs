//! Grouping sequencing files into samples by a [`DetectRule`]: the automatic reading of common
//! naming schemes, the name parts a person picked, or a regex.

use std::collections::{BTreeMap, HashMap, HashSet};

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::samples::names::{ends_with_ignore_case, loose, natural_cmp, tokenize};
use crate::samples::stem;
use crate::text::{basename, parent};

/// A file to place in a sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DetectFile {
    pub uri: String,
    /// Reserved: callers may send the object's size; detection does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

/// What a sample id is read from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdFrom {
    /// The file name alone.
    #[default]
    Name,
    /// The parent folder's name, then the file name (for deliveries with one folder per run).
    NameAndParent,
}

/// How a file name becomes a sample id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IdMethod {
    /// Recognize the common schemes: Illumina `<id>_S1_L001_R1_001`, `<id>_R1`, `<id>_1`.
    #[default]
    Auto,
    /// Join the chosen parts of the name (indices into the response's `tokens`) into the id.
    Tokens { selected: Vec<usize> },
    /// A regular expression whose first capture group (or group `sample`) is the id, matched
    /// against the whole file name, extension included (`^(P\d+)_` matches; `^(P\d+)$` never
    /// does).
    Regex { pattern: String },
}

/// What to do with one sample's files from several lanes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lanes {
    /// One sample with every lane's files (technical replicates).
    #[default]
    Merge,
    /// One sample per lane (`<id>_L001`).
    Separate,
    /// A problem to fix: the workflow takes one file per read.
    Error,
}

/// A detection rule.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DetectRule {
    #[serde(default)]
    pub id_from: IdFrom,
    #[serde(default)]
    pub method: IdMethod,
    /// What to do with several lanes. When not given, the samplesheet's choice, and without a
    /// samplesheet, `merge`; a given `merge` wins over the samplesheet's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lanes: Option<Lanes>,
}

/// One detected sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DetectedSample {
    pub id: String,
    /// Read-1 files (several when lanes merge), lane by lane and in name order within a lane.
    pub read1: Vec<String>,
    pub read2: Vec<String>,
    /// How many lanes the files came from (1 when names carry none).
    pub lanes: usize,
    /// What looks wrong with this sample, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

/// Two samples that look like one pair split by a naming slip (`P7` has only read 1, `P07` only
/// read 2): the form offers to merge them into `into`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MergeSuggestion {
    pub ids: [String; 2],
    pub into: String,
}

/// The result of [`detect`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Detection {
    pub samples: Vec<DetectedSample>,
    /// Files the rule could not read an id from (a regex that did not match, a part index past the
    /// end of the name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
    /// Files left out on purpose: an uncompressed file whose gzipped copy is also given, a repeat of
    /// a file already given, and an Illumina index read (`_I1_001`, `_I2_001`), which is no sample's
    /// read 1 or read 2.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<MergeSuggestion>,
}

/// Why a [`DetectRule`]'s regex cannot detect samples ([`detect`]). The message reaches the person
/// who wrote the pattern, through the launch form.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SampleDetectError {
    /// The pattern does not compile; the regex parser's reason, on one line.
    #[error("invalid regular expression: {0}")]
    BadRegex(String),
    /// The pattern has no group to read the sample id from.
    #[error(
        "the regular expression needs a capture group for the sample id (one named `sample`, else \
         the first group)"
    )]
    NoGroup,
}
/// A file's parent folder name, or `None` for a file at a bucket's root (whose "parent" is the
/// bucket) or with no folder at all.
pub(super) fn folder_name(uri: &str) -> Option<&str> {
    let path = uri
        .split_once("://")
        .map_or(uri, |(_, rest)| rest.split_once('/').map_or("", |(_bucket, key)| key));
    let mut parts = path.trim_end_matches('/').rsplit('/');
    parts.next();
    parts.next().filter(|p| !p.is_empty())
}
/// The read of a stem that is only a read marker (`r1`, `R2`, `read_2`, and a bare `1` or `2` when
/// `bare` allows it): such files are named by their folder.
fn marker_read(stem: &str, bare: bool) -> Option<u8> {
    static MARKER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = MARKER.get_or_init(|| Regex::new(r"(?i)^(r|read|read_|r_)?([12])$").expect("valid"));
    let c = re.captures(stem)?;
    if c.get(1).is_none() && !bare {
        return None;
    }
    Some(if &c[2] == "2" { 2 } else { 1 })
}

/// Whether a stem is an Illumina index read (`P01_S1_L001_I1_001`).
fn is_index_read(stem: &str) -> bool {
    static INDEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| Regex::new(r"(?i)_I[12]_\d{3}$").expect("valid")).is_match(stem)
}
/// Read number and lane read off a stem by the automatic schemes, and the id they leave.
pub(super) struct Reading {
    id: String,
    read: u8,
    pub(super) lane: Option<String>,
}

fn auto_patterns() -> &'static [Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Illumina bcl2fastq/BCL Convert: <id>_S<n>[_L<lane>]_R<read>_001
            r"^(?P<id>.+?)_S\d+(?:_L(?P<lane>\d{3}))?_R(?P<read>[12])(?:_\d{3})?$",
            // <id>[_L<lane>]_R<read>[_001]
            r"^(?P<id>.+?)(?:_L(?P<lane>\d{3}))?[._-]R(?P<read>[12])(?:_\d{3})?$",
            // <id>_1 / <id>.2
            r"^(?P<id>.+?)[._-](?P<read>[12])$",
        ]
        .iter()
        .map(|p| Regex::new(&format!("(?i){p}")).expect("valid pattern"))
        .collect()
    })
}

pub(super) fn auto_read(stem: &str) -> Reading {
    for re in auto_patterns() {
        if let Some(c) = re.captures(stem) {
            return Reading {
                id: c["id"].to_string(),
                read: if &c["read"] == "2" { 2 } else { 1 },
                lane: c.name("lane").map(|m| m.as_str().to_string()),
            };
        }
    }
    Reading { id: stem.to_string(), read: 1, lane: None }
}

enum Compiled {
    Auto,
    Tokens(Vec<usize>),
    Regex(Regex, usize),
}

fn compile(method: &IdMethod) -> Result<Compiled, SampleDetectError> {
    Ok(match method {
        IdMethod::Auto => Compiled::Auto,
        IdMethod::Tokens { selected } => {
            let mut s = selected.clone();
            s.sort_unstable();
            s.dedup();
            Compiled::Tokens(s)
        }
        IdMethod::Regex { pattern } => {
            let re = Regex::new(pattern)
                .map_err(|e| SampleDetectError::BadRegex(crate::text::regex_error_reason(&e)))?;
            let group = re.capture_names().position(|n| n == Some("sample")).unwrap_or(1);
            if re.captures_len() <= group {
                return Err(SampleDetectError::NoGroup);
            }
            Compiled::Regex(re, group)
        }
    })
}

/// The id, read and lane of one file under the rule; `None` when the rule does not apply to it.
/// `bare_markers` says a bare `1`/`2` name is a read marker (its folder holds no other numbers).
fn read_file(
    uri: &str,
    rule: &DetectRule,
    method: &Compiled,
    bare_markers: bool,
) -> Option<Reading> {
    let name = basename(uri);
    let mut auto = auto_read(stem(name));
    // A name that is only a read marker takes its sample id from its folder (automatic ids only;
    // at a bucket root it stays a single-end sample named by its stem).
    let folder_id = match (marker_read(stem(name), bare_markers), folder_name(uri)) {
        (Some(read), Some(folder)) => {
            auto.read = read;
            Some(folder.to_string())
        }
        _ => None,
    };
    let id = match method {
        Compiled::Auto if folder_id.is_some() => {
            // Already the folder name: don't prefix it again.
            return folder_id.map(|id| Reading { id, read: auto.read, lane: None });
        }
        Compiled::Auto => auto.id.clone(),
        Compiled::Tokens(selected) => {
            let tokens = tokenize(name);
            if selected.is_empty() || selected.iter().any(|&i| i >= tokens.len()) {
                return None;
            }
            selected.iter().map(|&i| tokens[i].text.as_str()).collect()
        }
        Compiled::Regex(re, group) => re.captures(name)?.get(*group)?.as_str().to_string(),
    };
    if id.is_empty() {
        return None;
    }
    let id = match rule.id_from {
        IdFrom::Name => id,
        // A file at a bucket's root has no folder to prefix (the bucket is not one).
        IdFrom::NameAndParent => match folder_name(uri) {
            None => id,
            Some(parent) => format!("{parent}_{id}"),
        },
    };
    Some(Reading { id, read: auto.read, lane: auto.lane })
}

/// Each file name that more than one of `uris` (sorted by name) ends in, with how many do.
fn repeated_names(uris: &[String]) -> Vec<(&str, usize)> {
    let mut out: Vec<(&str, usize)> = Vec::new();
    for uri in uris {
        let name = basename(uri);
        match out.last_mut() {
            Some((last, n)) if *last == name => *n += 1,
            _ => out.push((name, 1)),
        }
    }
    out.retain(|(_, n)| *n > 1);
    out
}

/// Group `files` into samples by `rule`. Every file lands in exactly one sample, or in
/// `unmatched`/`skipped`.
pub fn detect(files: &[DetectFile], rule: &DetectRule) -> Result<Detection, SampleDetectError> {
    let method = compile(&rule.method)?;
    let lanes_rule = rule.lanes.unwrap_or_default();
    let mut out = Detection::default();

    // A gzipped copy wins over the same file uncompressed; a file given twice is read once; an
    // index read is no sample's read.
    let names: HashSet<&str> = files.iter().map(|f| f.uri.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut kept: Vec<&str> = Vec::new();
    for f in files {
        let uri = f.uri.as_str();
        let shadowed = (ends_with_ignore_case(uri, ".fastq") || ends_with_ignore_case(uri, ".fq"))
            && names.contains(format!("{uri}.gz").as_str());
        if shadowed || !seen.insert(uri) || is_index_read(stem(basename(uri))) {
            out.skipped.push(uri.to_string());
        } else {
            kept.push(uri);
        }
    }

    // A bare `1`/`2` name is a read marker only in a folder with no other numbered names: in
    // `run/{1,2,3}.fastq.gz` the numbers are samples, not reads.
    let numbered_folders: HashSet<&str> = kept
        .iter()
        .filter(|u| {
            let s = stem(basename(u));
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s != "1" && s != "2"
        })
        .map(|u| parent(u))
        .collect();

    // id → (lane → (read1, read2))
    type LaneReads = (Vec<String>, Vec<String>);
    type ByLane = BTreeMap<Option<String>, LaneReads>;
    let mut groups: BTreeMap<String, ByLane> = BTreeMap::new();
    for uri in kept {
        let bare = !numbered_folders.contains(parent(uri));
        let Some(r) = read_file(uri, rule, &method, bare) else {
            out.unmatched.push(uri.to_string());
            continue;
        };
        let entry = groups.entry(r.id).or_default().entry(r.lane).or_default();
        if r.read == 2 {
            entry.1.push(uri.to_string());
        } else {
            entry.0.push(uri.to_string());
        }
    }

    // (sample, whether its id was made by splitting lanes)
    let mut built: Vec<(DetectedSample, bool)> = Vec::new();
    for (id, by_lane) in groups {
        // Illumina's reads that matched no sample's index (also under a folder prefix).
        let undetermined =
            id.eq_ignore_ascii_case("undetermined") || ends_with_ignore_case(&id, "_undetermined");
        let mut units: Vec<(String, bool, ByLane)> = Vec::new();
        if lanes_rule == Lanes::Separate && by_lane.len() > 1 {
            for (lane, reads) in by_lane {
                let (sid, split) = match &lane {
                    Some(l) => (format!("{id}_L{l}"), true),
                    None => (id.clone(), false),
                };
                units.push((sid, split, BTreeMap::from([(lane, reads)])));
            }
        } else {
            units.push((id, false, by_lane));
        }
        for (sid, split, mut lanes) in units {
            let lane_count = lanes.len();
            // Lane by lane, in name order within a lane, so the n-th read 1 and read 2 of a lane
            // are mates. One name in two folders orders by folder in both lists, so those mates
            // stay together too.
            let by_name = |a: &String, b: &String| {
                natural_cmp(basename(a), basename(b)).then_with(|| a.cmp(b))
            };
            for (r1, r2) in lanes.values_mut() {
                r1.sort_by(by_name);
                r2.sort_by(by_name);
            }
            let read1: Vec<String> = lanes.values().flat_map(|l| l.0.iter().cloned()).collect();
            let read2: Vec<String> = lanes.values().flat_map(|l| l.1.iter().cloned()).collect();
            let mut problems = Vec::new();
            if read1.is_empty() {
                problems.push("read 2 without read 1".to_string());
            } else if !read2.is_empty() {
                if lane_count == 1 {
                    if read1.len() != read2.len() {
                        problems.push(format!(
                            "{} read-1 files but {} read-2 files",
                            read1.len(),
                            read2.len()
                        ));
                    }
                } else {
                    // Mates pair within a lane, so each lane must balance on its own.
                    for (lane, (r1, r2)) in &lanes {
                        if r1.len() != r2.len() {
                            let lane = lane
                                .as_ref()
                                .map_or("files with no lane".to_string(), |l| format!("lane {l}"));
                            problems.push(format!(
                                "{lane}: {} read-1 files but {} read-2 files",
                                r1.len(),
                                r2.len()
                            ));
                        }
                    }
                }
            }
            if lanes_rule == Lanes::Error && lane_count > 1 {
                problems.push(format!("{lane_count} lanes; this workflow takes one file per read"));
            }
            // Whatever the lanes rule: several read-1 files with no lane to tell them apart (the
            // same name in two folders, or two spellings of one read), and, within a lane, one
            // name in two folders. Distinct names in one lane are its split parts (`_001`,
            // `_002`), which are ordinary.
            for (lane, (r1, _)) in &lanes {
                match lane {
                    None if r1.len() > 1 => {
                        let what = format!("{} read-1 files for one sample", r1.len());
                        problems.push(if lane_count == 1 {
                            what
                        } else {
                            format!("files with no lane: {what}")
                        });
                    }
                    None => {}
                    Some(l) => {
                        for (name, n) in repeated_names(r1) {
                            problems.push(format!("lane {l}: {n} read-1 files named {name}"));
                        }
                    }
                }
            }
            if undetermined {
                problems.push(
                    "undetermined reads: demultiplexing matched them to no sample".to_string(),
                );
            }
            built.push((
                DetectedSample { id: sid, read1, read2, lanes: lane_count, problems },
                split,
            ));
        }
    }

    // A lane-split id (`A` on lane 001 → `A_L001`) can equal a sample's own id. Split ids never
    // equal each other, so the split one is renamed, never merged: the two hold different reads.
    let own: HashSet<String> =
        built.iter().filter(|(_, split)| !split).map(|(s, _)| s.id.clone()).collect();
    let mut taken: HashSet<String> = built.iter().map(|(s, _)| s.id.clone()).collect();
    for (s, split) in &mut built {
        if *split && own.contains(&s.id) {
            let renamed = (2..)
                .map(|n| format!("{}_{n}", s.id))
                .find(|c| !taken.contains(c))
                .expect("an unused name");
            s.problems.push(format!("named {renamed}: another sample is already named {}", s.id));
            taken.insert(renamed.clone());
            s.id = renamed;
        }
    }
    out.samples = built.into_iter().map(|(s, _)| s).collect();
    out.samples.sort_by(|a, b| natural_cmp(&a.id, &b.id));

    // A read-1-only sample and a read-2-only sample whose ids match loosely are probably one pair.
    let mut only2: HashMap<String, &DetectedSample> = HashMap::new();
    for s in out.samples.iter().filter(|s| s.read1.is_empty() && !s.read2.is_empty()) {
        only2.entry(loose(&s.id)).or_insert(s);
    }
    let mut suggestions = Vec::new();
    for a in out.samples.iter().filter(|s| !s.read1.is_empty() && s.read2.is_empty()) {
        if let Some(b) = only2.get(&loose(&a.id)) {
            // Keep the longer spelling (usually the zero-padded one the rest of the batch uses).
            let into = if b.id.len() >= a.id.len() { &b.id } else { &a.id };
            suggestions
                .push(MergeSuggestion { ids: [a.id.clone(), b.id.clone()], into: into.clone() });
        }
    }
    out.suggestions = suggestions;
    Ok(out)
}
