//! Which sample a task belongs to: the [`SampleMatcher`] a run's [`SampleSpec`] builds, asked once
//! per task key.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{SampleHint, SampleSpec, StepScope};

/// Wildcard names taken to name the sample when the author gave no hint, in this order.
const SAMPLE_WILDCARDS: [&str; 5] = ["sample", "sample_id", "sample_name", "sm", "id"];

/// File-name endings (lowercase, without the leading dot) that mark a Nextflow tag as a file rather
/// than a sample (`GUNZIP (genome.fa.gz)`) when the run has no sheet to check tags against. A
/// `samples.tag` hint overrides this.
const FILE_TAG_EXTENSIONS: &[&str] = &[
    "fa", "fasta", "fna", "gtf", "gff", "gff3", "bed", "gz", "bam", "cram", "vcf", "txt", "tsv",
    "csv", "json", "dict", "fai",
];

/// The value `map` holds for `group`: under its full name, else (a Nextflow process path) under its
/// last `:`-separated part. The one way a hint's `steps` and `labels` name a step.
pub(super) fn by_step<'m, V>(map: &'m BTreeMap<String, V>, group: &str) -> Option<&'m V> {
    map.get(group).or_else(|| group.rsplit_once(':').and_then(|(_, short)| map.get(short)))
}

/// The scope `hint` forces on `group`, if any (an unknown scope forces nothing).
pub(super) fn forced<'h>(hint: &'h SampleHint, group: &str) -> Option<&'h StepScope> {
    by_step(&hint.steps, group).filter(|s| !matches!(s, StepScope::Unknown(_)))
}

/// How the run's `samples.tag` hint reads a tag.
enum TagRule {
    /// No hint: the default rules.
    Default,
    /// The hint's regex; its `sample` group is the sample.
    Pattern(regex::Regex),
    /// A hint that does not compile names nothing (the register step warns about it).
    Invalid,
}

/// Attributes tasks to samples under one [`SampleSpec`]: built once per run (the id set and the
/// tag regex are resolved here), then asked per task.
///
/// Snakemake keys are `rule:k1=v1,k2=v2`: the hinted wildcard, else the first of
/// `SAMPLE_WILDCARDS` present, else the one wildcard whose value is a sheet id. Nextflow keys are
/// `process (tag) #index` (or `process (index)` untagged): the hint's regex over the tag, else the
/// tag itself (with no sheet: unless it looks like a file name), else the one sheet id among the
/// tag's tokens (split on anything but letters, digits, `-` and `_`, and each also on `_`).
///
/// With a sheet (non-empty `ids`) the default rules name only the sheet's ids; a hinted wildcard
/// or tag regex is the author's explicit choice and is trusted as given.
pub struct SampleMatcher<'a> {
    hint: &'a SampleHint,
    ids: BTreeSet<&'a str>,
    tag: TagRule,
}

impl<'a> SampleMatcher<'a> {
    pub fn new(spec: &'a SampleSpec) -> SampleMatcher<'a> {
        let tag = match spec.hint.tag.as_deref() {
            None => TagRule::Default,
            Some(p) => regex::Regex::new(p).map_or(TagRule::Invalid, TagRule::Pattern),
        };
        SampleMatcher { hint: &spec.hint, ids: spec.ids.iter().map(String::as_str).collect(), tag }
    }

    /// The sample a task belongs to, or `None` for a whole-study (or unmatchable) task.
    pub fn attribute(&self, task_key: &str, group: Option<&str>) -> Option<String> {
        if group.and_then(|g| forced(self.hint, g)) == Some(&StepScope::Study) {
            return None;
        }
        let sample = match nextflow_key(task_key) {
            Some(Some(tag)) => self.sample_of_tag(tag),
            Some(None) => None,
            None => self.sample_of_wildcards(task_key),
        };
        sample.filter(|s| !s.is_empty())
    }

    fn sample_of_tag(&self, tag: &str) -> Option<String> {
        match &self.tag {
            TagRule::Pattern(re) => {
                return re
                    .captures(tag)
                    .and_then(|c| c.name("sample"))
                    .map(|m| m.as_str().to_string());
            }
            TagRule::Invalid => return None,
            TagRule::Default => {}
        }
        if self.ids.is_empty() {
            return (!looks_like_a_file(tag)).then(|| tag.to_string());
        }
        if self.ids.contains(tag) {
            return Some(tag.to_string());
        }
        let mut found: BTreeSet<&str> = tag
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .flat_map(|t| std::iter::once(t).chain(t.split('_')))
            .filter(|t| self.ids.contains(*t))
            .collect();
        // A sheet id the split cannot give whole (`NA12878.1`) counts where it stands alone in the
        // tag, and then wins over an id it contains (`NA12878`).
        for id in self.ids.iter().filter(|id| !is_token(id)) {
            if stands_alone(tag, id) {
                found.retain(|t| !id.contains(*t));
                found.insert(id);
            }
        }
        one(found)
    }

    fn sample_of_wildcards(&self, key: &str) -> Option<String> {
        let (_, inner) = key.split_once(':')?;
        let wildcards: BTreeMap<&str, &str> =
            inner.split(',').filter_map(|kv| kv.split_once('=')).collect();
        if let Some(name) = self.hint.wildcard.as_deref() {
            return wildcards.get(name).map(std::string::ToString::to_string);
        }
        let named = SAMPLE_WILDCARDS.iter().find_map(|w| wildcards.get(w));
        if let Some(v) = named.filter(|v| self.ids.is_empty() || self.ids.contains(**v)) {
            return Some(v.to_string());
        }
        one(wildcards.values().copied().filter(|v| self.ids.contains(v)).collect())
    }
}

/// [`SampleMatcher::attribute`] for one task (tests and one-off reads; build a [`SampleMatcher`]
/// once to attribute many).
pub fn attribute(task_key: &str, group: Option<&str>, spec: &SampleSpec) -> Option<String> {
    SampleMatcher::new(spec).attribute(task_key, group)
}

/// Read a Nextflow key: `Some(Some(tag))` for `process (tag) #index`, `Some(None)` for an untagged
/// `process (index)`, `None` for a Snakemake key. In the tagged form the tag is the tag even when
/// it is all digits (a numeric sample id); only the untagged form's all-digit inner is an index. A
/// key whose part before ` (` holds `=` is a Snakemake key whose wildcard value contains ` (`.
fn nextflow_key(key: &str) -> Option<Option<&str>> {
    let open = key.find(" (")?;
    if key[..open].contains('=') {
        return None;
    }
    let rest = &key[open + 2..];
    if let Some(close) = rest.rfind(") #") {
        let index = &rest[close + 3..];
        if !index.is_empty() && index.chars().all(|c| c.is_ascii_digit()) {
            return Some(Some(&rest[..close]));
        }
    }
    let inner = rest.strip_suffix(')')?;
    Some((!inner.chars().all(|c| c.is_ascii_digit())).then_some(inner))
}

/// Whether a tag names a file (ends in one of [`FILE_TAG_EXTENSIONS`], case-insensitively).
fn looks_like_a_file(tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    FILE_TAG_EXTENSIONS
        .iter()
        .any(|ext| tag.strip_suffix(ext).is_some_and(|stem| stem.len() > 1 && stem.ends_with('.')))
}

/// Whether `id` is one token of a tag's split (letters, digits, `-` and `_` only).
fn is_token(id: &str) -> bool {
    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether `id` occurs in `tag` with no letter or digit right before or after it.
fn stands_alone(tag: &str, id: &str) -> bool {
    tag.match_indices(id).any(|(at, _)| {
        let before = tag[..at].chars().next_back();
        let after = tag[at + id.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// The single member of `set`, or `None` for none or several.
fn one(set: BTreeSet<&str>) -> Option<String> {
    let mut it = set.into_iter();
    match (it.next(), it.next()) {
        (Some(only), None) => Some(only.to_string()),
        _ => None,
    }
}
