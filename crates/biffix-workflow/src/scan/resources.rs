//! Which rules of a Snakemake workflow would run on AWS Batch with Snakemake's default memory,
//! read from the source (nothing is executed), for `inspect` and `register` to warn about.
//!
//! The run is the coordinator's: Snakemake started from a work directory outside the source, with
//! `--snakefile <source>/<entrypoint>` and no `--profile` or `--workflow-profile`. What follows is
//! Snakemake 9's behaviour there (checked against its source and by running 9.0.1, 9.16.2,
//! 9.17.3, 9.21.0 and 9.27.0):
//!
//! - **Default memory.** For a remote executor Snakemake applies its full default resources
//!   (`api.py`), whose memory is `min(max(2*input.size_mb, 1000), 8000)` MB (`resources.py`).
//!   The AWS Batch executor sizes the container from `mem_mb` alone.
//! - **Memory keys.** `mem_mb` always sets it. From 9.16.3 `mem` and `mem_mib` are normalized into
//!   it too (`Resources._normalize_sizes`); before that only a string `mem` (`"8G"`) was converted
//!   (`infer_resources`). `mem_gb` and `mem_gib` are never read as memory.
//! - **The workflow profile** is `profiles/default/` beside the Snakefile, and only that one (the
//!   other candidates are relative to the work directory). Snakemake reads exactly one file from
//!   it: among the `config.yaml` names (from 9.19 also `profile.yaml`, matched whole), with an
//!   optional `.vN+` infix, the one with the highest `N` not above the running major version.
//!   There, `default-resources` (or its alias `default-res`, or an unambiguous prefix of
//!   `default-resources`) sizes every rule, and `set-resources` (no abbreviation is unambiguous)
//!   sizes the rules it names. Only `default-resources` itself may be a mapping; any other spelling
//!   (`default_resources`) is refused or ignored. A plain number for `mem` there is megabytes only
//!   from 9.20.0 (9.16.3 to 9.19 read bytes; earlier releases did not read it as memory).
//! - **Rule files** are the Snakefile and what it `include:`s, each path relative to the including
//!   file.
//!
//! When the scan cannot tell, it errs toward a rule being sized, so the warning does not cry wolf:
//! a dynamic `resources: **helper`, a memory value it cannot read, an `include:` whose path is not
//! a plain string (its rules, and any `localrules:` in it, are unseen), a `use rule … as …` whose
//! base rule it did not see, and rules imported from a `module` (`use rule … from …`), which are
//! named in [`ResourceScan::unchecked_modules`] instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;

use super::{lex_python, skip_ws, word_positions};
use crate::engines::EngineVersion;

/// Snakemake normalizes `mem` and `mem_mib` into `mem_mb` from this release.
const NORMALIZES_MEM: EngineVersion = EngineVersion::new(9, 16, 3);
/// A workflow profile may be named `profile.yaml` (and the name must match whole) from this release.
const PROFILE_YAML_NAMES: EngineVersion = EngineVersion::new(9, 19, 0);
/// A plain number for a profile's `mem` is megabytes from this release.
const PROFILE_MEM_NUMBER_IS_MB: EngineVersion = EngineVersion::new(9, 20, 0);
/// The oldest supported release and the first of each behaviour above: what a workflow with no
/// pinned engine version is checked against (a rule is listed when any of them would leave it on
/// the default memory).
const BEHAVIOURS: [EngineVersion; 4] =
    [EngineVersion::new(9, 0, 0), NORMALIZES_MEM, PROFILE_YAML_NAMES, PROFILE_MEM_NUMBER_IS_MB];

/// The resource keys that look like memory; which of them Snakemake reads depends on its version.
const MEM_LIKE_KEYS: [&str; 5] = ["mem_mb", "mem_mib", "mem", "mem_gb", "mem_gib"];

/// The rule directives that make a rule run a job (one without any of them never reaches AWS
/// Batch, like an `all` target that only lists inputs).
const JOB_DIRECTIVES: [&str; 7] =
    ["shell", "run", "script", "notebook", "wrapper", "cwl", "template_engine"];

/// A rule that would run with Snakemake's default memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleLoc {
    /// The file, relative to the source root.
    pub path: String,
    /// The 1-based line of the `rule`/`checkpoint` header (or of the `use rule` statement).
    pub line: usize,
    /// The rule's name (`<anonymous>` for a `rule:` without one).
    pub rule: String,
    /// A memory-like key the rule sets that Snakemake does not read as memory (`mem_gb`, or
    /// `mem_mib` before 9.16.3), if any: likely what the author meant.
    pub ignored_key: Option<String>,
}

/// The rules of a Snakemake workflow that would run on AWS Batch with Snakemake's default memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceScan {
    /// Rules that run a job on AWS Batch and get no memory from their `resources:` or the workflow
    /// profile, in path and line order. Empty when the profile's `default-resources` sizes every
    /// rule.
    pub rules_without_mem: Vec<RuleLoc>,
    /// Where to set `default-resources`: the workflow-profile file Snakemake reads, else
    /// `<snakefile dir>profiles/default/config.yaml`.
    pub profile_hint: String,
    /// The workflow-profile file Snakemake reads, when it could not be parsed (taken to set
    /// nothing; Snakemake itself refuses a profile it cannot parse).
    pub unreadable_profile: Option<String>,
    /// Modules whose rules are imported (`use rule … from <module>`): not checked.
    pub unchecked_modules: Vec<String>,
    /// No engine version was given, so every supported Snakemake 9 behaviour was assumed.
    pub every_release: bool,
}

/// Find the rules of a Snakemake workflow that would run on AWS Batch with Snakemake's default
/// memory (see the module docs for the rules followed).
///
/// `paths` are the source's files (relative to its root) and `read` returns one's text (`None`
/// when it cannot be read; only the Snakefile, the files it includes and the workflow-profile
/// files are read). `entrypoint` is the Snakefile the run uses. `engine_version` is the Snakemake
/// release the workflow pins; with none, a rule is listed when any supported release would leave
/// it on the default memory.
pub fn scan_snakemake_resources<R>(
    paths: &[&str],
    read: R,
    entrypoint: &str,
    engine_version: Option<EngineVersion>,
) -> ResourceScan
where
    R: FnMut(&str) -> Option<String>,
{
    let mut source = Source {
        paths: paths.iter().map(std::string::ToString::to_string).collect(),
        read,
        texts: BTreeMap::new(),
    };
    let snakefile = entrypoint.trim_start_matches("./");
    let dir = crate::text::parent(snakefile);
    let profile_dir = format!("{dir}profiles/default/");

    let workflow = read_workflow(&mut source, snakefile);
    let versions = engine_version.map_or_else(|| BEHAVIOURS.to_vec(), |v| vec![v]);
    let mut listed: BTreeMap<(String, usize, String), RuleLoc> = BTreeMap::new();
    let mut profile_hint = None;
    let mut unreadable_profile = None;
    for version in versions {
        let profile = profile_view(&mut source, &profile_dir, version);
        // The newest behaviour's file names the hint (versions run oldest first), else the
        // default `config.yaml`: a file only an older release reads is not where to add a default.
        profile_hint.clone_from(&profile.file);
        unreadable_profile = unreadable_profile.or(profile.unreadable);
        if profile.default_mem {
            continue;
        }
        for rule in &workflow.rules {
            if !rule.body.runs_job
                || rule.body.localrule == Some(true)
                || workflow.local.contains(&rule.name)
                || profile.set_mem.contains(&rule.name)
            {
                continue;
            }
            let resources = rule.body.resources.as_deref().unwrap_or("");
            if resources_size_memory(resources, version) {
                continue;
            }
            listed.entry((rule.path.clone(), rule.line, rule.name.clone())).or_insert_with(|| {
                RuleLoc {
                    path: rule.path.clone(),
                    line: rule.line,
                    rule: rule.name.clone(),
                    ignored_key: ignored_mem_key(resources, version),
                }
            });
        }
    }
    ResourceScan {
        rules_without_mem: listed.into_values().collect(),
        profile_hint: profile_hint.unwrap_or_else(|| format!("{profile_dir}config.yaml")),
        unreadable_profile,
        unchecked_modules: workflow.modules.into_iter().collect(),
        every_release: engine_version.is_none(),
    }
}

/// The source's files, read on demand and at most once.
struct Source<R> {
    paths: BTreeSet<String>,
    read: R,
    texts: BTreeMap<String, Option<String>>,
}

impl<R: FnMut(&str) -> Option<String>> Source<R> {
    fn text(&mut self, path: &str) -> Option<&str> {
        if !self.paths.contains(path) {
            return None;
        }
        if !self.texts.contains_key(path) {
            let text = (self.read)(path);
            self.texts.insert(path.to_string(), text);
        }
        self.texts[path].as_deref()
    }
}

// ---------------------------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------------------------

/// What a rule (or a `use rule … with:` block) says about running a job and its memory.
#[derive(Clone, Debug, Default)]
struct Body {
    runs_job: bool,
    /// The `resources:` text (strings' bodies and comments blanked, quotes kept).
    resources: Option<String>,
    /// `localrule: True`/`False`, when set.
    localrule: Option<bool>,
}

/// One statement of a rule file that matters here, in source order.
enum Statement {
    Rule {
        line: usize,
        name: String,
        body: Body,
    },
    /// `use rule <base> as <alias> [with: …]`, one `(base, alias)` per rule named.
    Derived {
        line: usize,
        names: Vec<(String, String)>,
        with: Option<Body>,
    },
    /// `use rule … from <module>`.
    Imported {
        module: String,
    },
    LocalRules(Vec<String>),
    /// An `include:` of a plain string path, resolved against the including file (`None` when the
    /// path is not a plain string, or names no file of the source).
    Include(Option<String>),
}

/// A rule as the run sees it (a derived rule with its base's directives merged in).
struct Rule {
    path: String,
    line: usize,
    name: String,
    body: Body,
}

/// The rules a workflow defines, the rules `localrules:` lists, and the modules it imports from.
#[derive(Default)]
struct Workflow {
    rules: Vec<Rule>,
    local: BTreeSet<String>,
    modules: BTreeSet<String>,
}

/// Read the Snakefile and every file it includes (each once, in include order, as Snakemake runs
/// them), resolving each `use rule … as …` against the rules defined before it.
fn read_workflow<R: FnMut(&str) -> Option<String>>(
    source: &mut Source<R>,
    snakefile: &str,
) -> Workflow {
    let mut workflow = Workflow::default();
    let mut bases: BTreeMap<String, Body> = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut stack = vec![(snakefile.to_string(), None::<std::vec::IntoIter<Statement>>)];
    while let Some((path, statements)) = stack.last_mut() {
        let statements = match statements {
            Some(s) => s,
            None => {
                let parsed = match source.text(path) {
                    Some(text) if visited.insert(path.clone()) => parse_rule_file(path, text),
                    _ => Vec::new(),
                };
                statements.insert(parsed.into_iter())
            }
        };
        let Some(statement) = statements.next() else {
            stack.pop();
            continue;
        };
        let path = path.clone();
        match statement {
            Statement::Rule { line, name, body } => {
                bases.insert(name.clone(), body.clone());
                workflow.rules.push(Rule { path, line, name, body });
            }
            Statement::Derived { line, names, with } => {
                for (base, alias) in names {
                    // A base the scan did not see (a dynamic include, a module) is not checked.
                    let Some(base) = bases.get(&base) else {
                        continue;
                    };
                    let body = match &with {
                        Some(with) => Body {
                            runs_job: base.runs_job || with.runs_job,
                            // A `with:` block's `resources:` replaces the base's whole.
                            resources: with.resources.clone().or_else(|| base.resources.clone()),
                            localrule: with.localrule.or(base.localrule),
                        },
                        None => base.clone(),
                    };
                    bases.insert(alias.clone(), body.clone());
                    workflow.rules.push(Rule { path: path.clone(), line, name: alias, body });
                }
            }
            Statement::Imported { module } => {
                workflow.modules.insert(module);
            }
            Statement::LocalRules(names) => workflow.local.extend(names),
            Statement::Include(Some(included)) => {
                if !visited.contains(&included) {
                    stack.push((included, None));
                }
            }
            Statement::Include(None) => {}
        }
    }
    workflow
}

/// One line of code: its 1-based number, indentation and (trimmed, non-empty) masked text. A line
/// that continues the one before (inside a string or a bracket) has indentation `usize::MAX`, so
/// it never ends a block or reads as a directive.
struct CodeLine<'a> {
    no: usize,
    indent: usize,
    code: &'a str,
}

/// The index of the first line after `start` indented no deeper than `indent` (the end of the
/// block `start` opens).
fn block_end(lines: &[CodeLine], start: usize, indent: usize) -> usize {
    (start + 1..lines.len()).find(|i| lines[*i].indent <= indent).unwrap_or(lines.len())
}

/// A directive's whole text: what follows its `:` and the lines nested under it.
fn directive_text(lines: &[CodeLine], at: usize, rest: &str) -> String {
    let end = block_end(lines, at, lines[at].indent);
    std::iter::once(rest)
        .chain(lines[at + 1..end].iter().map(|l| l.code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn directive_re() -> &'static regex::Regex {
    static DIRECTIVE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    DIRECTIVE
        .get_or_init(|| regex::Regex::new(r"^([A-Za-z_]\w*)\s*:(.*)$").expect("a valid pattern"))
}

/// The directives of the block opened at `start` (a rule header or a `with:`), up to `end`.
fn parse_body(lines: &[CodeLine], start: usize, end: usize) -> Body {
    let mut body = Body::default();
    let Some(body_indent) = lines.get(start + 1).filter(|_| start + 1 < end).map(|l| l.indent)
    else {
        return body;
    };
    for j in start + 1..end {
        if lines[j].indent != body_indent {
            continue;
        }
        let Some(d) = directive_re().captures(lines[j].code) else {
            continue;
        };
        let rest = d.get(2).map_or("", |m| m.as_str());
        match &d[1] {
            "resources" => body.resources = Some(directive_text(lines, j, rest)),
            "localrule" => body.localrule = Some(rest.trim() == "True"),
            key => body.runs_job |= JOB_DIRECTIVES.contains(&key),
        }
    }
    body
}

/// The statements of one rule file at `path`.
fn parse_rule_file(path: &str, text: &str) -> Vec<Statement> {
    static HEADER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static USE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let header = HEADER.get_or_init(|| {
        regex::Regex::new(r"^(?:rule|checkpoint)(?:\s+([A-Za-z_]\w*))?\s*:")
            .expect("a valid pattern")
    });
    let use_rule =
        USE.get_or_init(|| regex::Regex::new(r"^use\s+rule\s+(.*)$").expect("a valid pattern"));
    let lexed = lex_python(text, true);
    let raw: Vec<&str> = text.lines().collect();
    let lines: Vec<CodeLine> = lexed
        .masked
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let code = l.trim();
            let continued = lexed.continued.get(i).copied().unwrap_or(false);
            (!code.is_empty()).then(|| CodeLine {
                no: i + 1,
                indent: if continued { usize::MAX } else { l.len() - l.trim_start().len() },
                code,
            })
        })
        .collect();
    let mut statements = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.indent == usize::MAX {
            continue;
        }
        if let Some(c) = header.captures(line.code) {
            let name =
                c.get(1).map_or_else(|| "<anonymous>".to_string(), |m| m.as_str().to_string());
            let body = parse_body(&lines, i, block_end(&lines, i, line.indent));
            statements.push(Statement::Rule { line: line.no, name, body });
        } else if let Some(c) = use_rule.captures(line.code) {
            statements.extend(parse_use_rule(&c[1], &lines, i, line.no));
        } else if let Some(d) = directive_re().captures(line.code) {
            match &d[1] {
                "localrules" => {
                    let names = directive_text(&lines, i, d.get(2).map_or("", |m| m.as_str()));
                    statements.push(Statement::LocalRules(
                        names
                            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                            .filter(|n| !n.is_empty())
                            .map(str::to_string)
                            .collect(),
                    ));
                }
                "include" => {
                    let target = raw
                        .get(line.no - 1)
                        .and_then(|l| include_literal(l))
                        .and_then(|rel| join_path(path, &rel));
                    statements.push(Statement::Include(target));
                }
                _ => {}
            }
        }
    }
    statements
}

/// `use rule a, b [from m] [exclude c] [as alias] [with:]` (the text after `use rule`), with its
/// `with:` block at `lines[at]`.
fn parse_use_rule(text: &str, lines: &[CodeLine], at: usize, line: usize) -> Option<Statement> {
    let text = text.replace([',', ':'], " ");
    let mut tokens = text.split_whitespace().peekable();
    let keyword = |t: &str| matches!(t, "from" | "as" | "exclude" | "with");
    let mut rules = Vec::new();
    while let Some(t) = tokens.next_if(|t| !keyword(t)) {
        rules.push(t.to_string());
    }
    let (mut module, mut alias, mut with) = (None, None, false);
    while let Some(t) = tokens.next() {
        match t {
            "from" => module = tokens.next().map(str::to_string),
            "as" => alias = tokens.next().map(str::to_string),
            "with" => with = true,
            // `exclude` names rules of the module: skip to the next keyword.
            _ => while tokens.next_if(|t| !keyword(t)).is_some() {},
        }
    }
    if let Some(module) = module {
        return Some(Statement::Imported { module });
    }
    let alias = alias?;
    let names = rules
        .into_iter()
        .filter(|r| r != "*")
        .map(|r| {
            let name = if alias.contains('*') { alias.replace('*', &r) } else { alias.clone() };
            (r, name)
        })
        .collect();
    let with = with.then(|| parse_body(lines, at, block_end(lines, at, lines[at].indent)));
    Some(Statement::Derived { line, names, with })
}

/// The path an `include:` line names, when it is one plain (or raw) string literal.
fn include_literal(line: &str) -> Option<String> {
    static INCLUDE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = INCLUDE.get_or_init(|| {
        regex::Regex::new(r#"^\s*include\s*:\s*[rR]?(?:"([^"\\]*)"|'([^'\\]*)')\s*(?:#.*)?$"#)
            .expect("a valid pattern")
    });
    let c = re.captures(line)?;
    c.get(1).or(c.get(2)).map(|m| m.as_str().to_string())
}

/// `rel` resolved against the directory of `from` (both root-relative); `None` for an absolute
/// path or one that leaves the source.
fn join_path(from: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.contains("://") {
        return None;
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for segment in rel.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

// ---------------------------------------------------------------------------------------------
// Memory keys
// ---------------------------------------------------------------------------------------------

/// The memory-like keys a `resources:` text assigns (`key=`, not `key==`), with the offset of each
/// value.
fn assignments(text: &str) -> impl Iterator<Item = (&'static str, usize)> + '_ {
    MEM_LIKE_KEYS.into_iter().flat_map(move |key| {
        word_positions(text, key).into_iter().filter_map(move |at| {
            let sep = skip_ws(text, at + key.len());
            (text.as_bytes().get(sep) == Some(&b'=') && text.as_bytes().get(sep + 1) != Some(&b'='))
                .then_some((key, sep + 1))
        })
    })
}

/// Whether the value at `at` is a plain number (`8000`, `8_000`, `1.5`) and nothing more.
fn is_number_at(text: &str, at: usize) -> bool {
    let start = skip_ws(text, at);
    let bytes = text.as_bytes();
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || matches!(bytes[end], b'_' | b'.')) {
        end += 1;
    }
    end > start
        && bytes[start].is_ascii_digit()
        && matches!(bytes.get(skip_ws(text, end)), None | Some(b',' | b')' | b'\n' | b'\r'))
}

/// Whether a rule's `key=value` sets `mem_mb` under `version` (a value the scan cannot read is
/// taken to).
fn rule_key_sizes(key: &str, text: &str, value_at: usize, version: EngineVersion) -> bool {
    match key {
        "mem_mb" => true,
        "mem_mib" => version >= NORMALIZES_MEM,
        // Before 9.16.3 only a string (`"8G"`) was converted; a number was not.
        "mem" => version >= NORMALIZES_MEM || !is_number_at(text, value_at),
        _ => false,
    }
}

/// Whether `text` has a `**` that unpacks a mapping (at the start, or after `,`, `(`, `{` or `:`,
/// whitespace aside), not one that raises to a power (`2**10`).
fn has_splat(text: &str) -> bool {
    text.match_indices("**").any(|(at, _)| {
        matches!(text[..at].trim_end().as_bytes().last(), None | Some(b',' | b'(' | b'{' | b':'))
    })
}

/// Whether a `resources:` text sizes memory under `version`: a memory key Snakemake reads, or
/// something the scan cannot see into (`**helper`), which is given the benefit of the doubt.
fn resources_size_memory(text: &str, version: EngineVersion) -> bool {
    has_splat(text) || assignments(text).any(|(key, at)| rule_key_sizes(key, text, at, version))
}

/// The first memory-like key `text` sets that does not size memory under `version`.
fn ignored_mem_key(text: &str, version: EngineVersion) -> Option<String> {
    assignments(text)
        .find(|(key, at)| !rule_key_sizes(key, text, *at, version))
        .map(|(key, _)| key.to_string())
}

// ---------------------------------------------------------------------------------------------
// The workflow profile
// ---------------------------------------------------------------------------------------------

/// What the workflow profile Snakemake reads under one version says about memory.
#[derive(Default)]
struct ProfileView {
    /// The file read (the first, when several tie).
    file: Option<String>,
    /// Its `default-resources` sizes every rule.
    default_mem: bool,
    /// The rules its `set-resources` sizes.
    set_mem: BTreeSet<String>,
    /// The file read, when it could not be parsed.
    unreadable: Option<String>,
}

/// The profile files in `dir` Snakemake would read under `version`: the eligible names with the
/// highest `.vN+` (several only on a tie, which Snakemake breaks by directory order).
fn profile_files<'a>(
    paths: &'a BTreeSet<String>,
    dir: &str,
    version: EngineVersion,
) -> Vec<&'a str> {
    static OLD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static NEW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // Python's `re.match` (anchored at the start only) before 9.19, `re.fullmatch` from it; the
    // unescaped dots are Snakemake's own.
    let name = if version >= PROFILE_YAML_NAMES {
        NEW.get_or_init(|| {
            regex::Regex::new(r"^(?:profile|config)(?:\.v(?P<min>\d+)\+)?.yaml$")
                .expect("a valid pattern")
        })
    } else {
        OLD.get_or_init(|| {
            regex::Regex::new(r"^config(.v(?P<min>\d+)\+)?.yaml").expect("a valid pattern")
        })
    };
    let eligible: Vec<(&str, u64)> = paths
        .iter()
        .filter_map(|p| {
            let file = p.strip_prefix(dir).filter(|f| !f.contains('/'))?;
            let c = name.captures(file)?;
            let min = c.name("min").map_or(Some(0), |m| m.as_str().parse().ok())?;
            (min <= u64::from(version.major)).then_some((p.as_str(), min))
        })
        .collect();
    let best = eligible.iter().map(|(_, min)| *min).max();
    eligible.into_iter().filter(|(_, min)| Some(*min) == best).map(|(p, _)| p).collect()
}

/// The workflow profile in `dir` as Snakemake reads it under `version`. On a tie, it sizes memory
/// only as far as every tied file does.
fn profile_view<R: FnMut(&str) -> Option<String>>(
    source: &mut Source<R>,
    dir: &str,
    version: EngineVersion,
) -> ProfileView {
    let files: Vec<String> =
        profile_files(&source.paths, dir, version).into_iter().map(str::to_string).collect();
    let Some(first) = files.first() else {
        return ProfileView::default();
    };
    let mut view =
        ProfileView { file: Some(first.clone()), default_mem: true, ..ProfileView::default() };
    for (n, file) in files.iter().enumerate() {
        let profile = source.text(file).and_then(Profile::parse);
        let Some(profile) = profile else {
            view.unreadable = view.unreadable.or_else(|| Some(file.clone()));
            view.default_mem = false;
            view.set_mem.clear();
            break;
        };
        view.default_mem &= profile.defaults.iter().any(|(k, v)| profile_key_sizes(k, v, version));
        let sized: BTreeSet<String> = profile
            .set
            .iter()
            .filter(|(_, k, v)| profile_key_sizes(k, v, version))
            .map(|(rule, _, _)| rule.clone())
            .collect();
        view.set_mem =
            if n == 0 { sized } else { view.set_mem.intersection(&sized).cloned().collect() };
    }
    view
}

/// Whether a profile's (or the command line's) `key=value` sets `mem_mb` under `version`.
fn profile_key_sizes(key: &str, value: &str, version: EngineVersion) -> bool {
    match key {
        "mem_mb" => true,
        "mem_mib" => version >= NORMALIZES_MEM,
        // A number is megabytes only from 9.20.0; a size such as `8G` always converts.
        "mem" => {
            let number = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
            !number || version >= PROFILE_MEM_NUMBER_IS_MB
        }
        _ => false,
    }
}

/// The parts of a workflow profile that set resources.
#[derive(Debug, Default, PartialEq)]
struct Profile {
    /// `default-resources` as `(key, value)`.
    defaults: Vec<(String, String)>,
    /// `set-resources` as `(rule, key, value)`.
    set: Vec<(String, String, String)>,
}

/// A YAML document's top-level mapping in order, a repeated key kept each time (PyYAML, which
/// Snakemake reads profiles with, keeps the last; `serde_norway::Mapping` refuses the repeat).
struct TopLevel(Vec<(serde_norway::Value, serde_norway::Value)>);

impl<'de> Deserialize<'de> for TopLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = TopLevel;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a mapping")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<TopLevel, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(TopLevel(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

impl Profile {
    /// Read a profile as Snakemake's `ProfileConfigFileParser` does; `None` when it is not a YAML
    /// mapping (Snakemake refuses such a profile). yte templating (`__use_yte__`) is not applied.
    fn parse(text: &str) -> Option<Profile> {
        use serde_norway::Value;
        let TopLevel(entries) = serde_norway::from_str(text).ok()?;
        let mut profile = Profile::default();
        for (key, value) in entries {
            let Some(key) = key.as_str() else { continue };
            // A null value is skipped, so it does not undo an earlier one.
            if value.is_null() {
                continue;
            }
            if is_default_resources(key) {
                profile.defaults = match &value {
                    // Only the full name gets the mapping form; under another spelling a mapping
                    // is dropped.
                    Value::Mapping(m) if key == "default-resources" => m
                        .iter()
                        .filter_map(|(k, v)| Some((k.as_str()?.to_string(), scalar_text(v)?)))
                        .collect(),
                    Value::Mapping(_) => Vec::new(),
                    other => key_values(other),
                };
            } else if key == "set-resources" {
                profile.set = match &value {
                    Value::Mapping(rules) => rules
                        .iter()
                        .filter_map(|(rule, resources)| {
                            Some((rule.as_str()?, resources.as_mapping()?))
                        })
                        .flat_map(|(rule, resources)| {
                            resources.iter().filter_map(move |(k, v)| {
                                Some((rule.to_string(), k.as_str()?.to_string(), scalar_text(v)?))
                            })
                        })
                        .collect(),
                    other => key_values(other)
                        .into_iter()
                        .filter_map(|(rule_key, v)| {
                            let (rule, k) = rule_key.split_once(':')?;
                            Some((rule.trim().to_string(), k.trim().to_string(), v))
                        })
                        .collect(),
                };
            }
        }
        Some(profile)
    }
}

/// Whether a profile key reaches `--default-resources`: the option, its alias `default-res`, or a
/// prefix of the option that only it has (argparse takes an unambiguous prefix; a shorter one also
/// matches the alias and is refused).
fn is_default_resources(key: &str) -> bool {
    key == "default-res"
        || (key.len() > "default-res".len() && "default-resources".starts_with(key))
}

/// A scalar's text as Snakemake formats it into `key=value`.
fn scalar_text(value: &serde_norway::Value) -> Option<String> {
    use serde_norway::Value;
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        _ => None,
    }
}

/// `key=value` items given as a list of strings, or as one string.
fn key_values(value: &serde_norway::Value) -> Vec<(String, String)> {
    use serde_norway::Value;
    let items: Vec<&str> = match value {
        Value::String(s) => s.split_whitespace().collect(),
        Value::Sequence(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const V9_0: EngineVersion = EngineVersion::new(9, 0, 0);
    const V9_21: EngineVersion = EngineVersion::new(9, 21, 0);
    const V9_27: EngineVersion = EngineVersion::new(9, 27, 0);

    /// Scan `files` with `entrypoint` and `version`.
    fn scan(
        files: &[(&str, &str)],
        entrypoint: &str,
        version: Option<EngineVersion>,
    ) -> ResourceScan {
        let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        let read = |p: &str| files.iter().find(|(f, _)| *f == p).map(|(_, t)| t.to_string());
        scan_snakemake_resources(&paths, read, entrypoint, version)
    }

    fn memless(scan: &ResourceScan) -> Vec<String> {
        scan.rules_without_mem
            .iter()
            .map(|r| match &r.ignored_key {
                Some(key) => format!("{}:{} {} ({key})", r.path, r.line, r.rule),
                None => format!("{}:{} {}", r.path, r.line, r.rule),
            })
            .collect()
    }

    /// Profile files (in `profiles/default/`), the release, and whether the profile covers memory.
    type ProfileCase<'a> = (Vec<(&'a str, &'a str)>, Option<EngineVersion>, bool);

    const RULE_A: &str = "rule a:\n    output: \"a\"\n    shell: \"x\"\n";

    #[test]
    fn rules_and_checkpoints_without_memory_are_listed() {
        let snakefile = r#"include: "rules/qc.smk"

rule all:
    input: "done.txt"

rule sized:
    output: "a"
    resources:
        mem_mb=4000,
    shell: "touch {output}"

rule inline_sized:
    output: "b"
    resources: runtime=10, mem="8G"
    shell: "touch {output}"

rule unsized:
    output: "c"
    resources:
        runtime=10,
    shell: "touch {output}"

checkpoint split:
    output: directory("d")
    shell: "mkdir {output}"

rule gib:
    output: "e"
    resources: mem_gib=2
    run:
        print("hi")

rule:
    output: "f"
    shell: "x"
"#;
        let qc = "rule fastqc:\n    output: \"q\"\n    resources:\n        mem_mb = 100\n    shell: \"fastqc\"\n\nrule multiqc:\n    output: \"m\"\n    script: \"scripts/mqc.py\"\n";
        let scan = scan(
            &[
                ("Snakefile", snakefile),
                ("rules/qc.smk", qc),
                // Not included: a test fixture, and a file that is not a rule file.
                ("tests/old.smk", RULE_A),
                (".test/Snakefile", RULE_A),
                ("README.md", "rule nope:\n    shell: \"x\"\n"),
            ],
            "Snakefile",
            Some(V9_27),
        );
        assert_eq!(
            memless(&scan),
            [
                "Snakefile:17 unsized",
                "Snakefile:23 split",
                "Snakefile:27 gib (mem_gib)",
                "Snakefile:33 <anonymous>",
                "rules/qc.smk:7 multiqc",
            ]
        );
        assert_eq!(scan.profile_hint, "profiles/default/config.yaml");
        assert!(scan.unchecked_modules.is_empty() && !scan.every_release);
    }

    #[test]
    fn which_memory_keys_count_depends_on_the_snakemake_release() {
        let snakefile = r#"rule mb:
    output: "a"
    resources: mem_mb=5000
    shell: "x"

rule mem_number:
    output: "b"
    resources: mem=5000
    shell: "x"

rule mem_size:
    output: "c"
    resources: mem="5G"
    shell: "x"

rule mib:
    output: "d"
    resources: mem_mib=5000
    shell: "x"

rule gb:
    output: "e"
    resources: mem_gb=5
    shell: "x"

rule mem_lambda:
    output: "f"
    resources: mem=lambda wc: 5000
    shell: "x"
"#;
        let files = [("Snakefile", snakefile)];
        // As Snakemake 9.0.1/9.16.2 and 9.17.3/9.21.0/9.27.0 size these rules (run, not guessed).
        let old = [
            "Snakefile:16 mib (mem_mib)",
            "Snakefile:21 gb (mem_gb)",
            "Snakefile:6 mem_number (mem)",
        ];
        let mut old_sorted = old.to_vec();
        old_sorted.sort();
        let mut got = memless(&scan(&files, "Snakefile", Some(EngineVersion::new(9, 16, 2))));
        got.sort();
        assert_eq!(got, old_sorted);
        for version in [NORMALIZES_MEM, V9_21, V9_27] {
            assert_eq!(
                memless(&scan(&files, "Snakefile", Some(version))),
                ["Snakefile:21 gb (mem_gb)"],
                "{version}"
            );
        }
        // No release given: any release that leaves a rule on the default lists it.
        let unknown = scan(&files, "Snakefile", None);
        assert!(unknown.every_release);
        let mut got = memless(&unknown);
        got.sort();
        assert_eq!(got, old_sorted);
    }

    #[test]
    fn local_rules_are_exempt() {
        let snakefile = "localrules: gather,\n    report\n\nrule gather:\n    output: \"g\"\n    shell: \"x\"\n\nrule report:\n    output: \"r\"\n    shell: \"x\"\n\nrule here:\n    output: \"h\"\n    localrule: True\n    shell: \"x\"\n\nrule remote:\n    output: \"o\"\n    localrule: False\n    shell: \"x\"\n";
        let scan = scan(&[("Snakefile", snakefile)], "Snakefile", Some(V9_27));
        assert_eq!(memless(&scan), ["Snakefile:17 remote"]);
    }

    #[test]
    fn comments_and_strings_do_not_size_or_hide_a_rule() {
        let snakefile = r#"rule commented:
    output: "a"
    resources:
        # mem_mb=4000,
        runtime=5,  # mem_mb=1
    shell: "x"

rule in_shell:
    output: "b"
    shell:
        """
echo 'mem_mb=1'
rule fake:
        """
    resources: mem_mb=10

rule docstring:
    """mem_mb=1"""
    output: "c"
    shell: "x"
"#;
        let scan = scan(&[("Snakefile", snakefile)], "Snakefile", Some(V9_27));
        assert_eq!(memless(&scan), ["Snakefile:1 commented", "Snakefile:17 docstring"]);
    }

    #[test]
    fn a_string_or_bracket_closed_at_column_zero_does_not_end_the_rule() {
        let snakefile = r#"rule closed_flush_left:
    output: "a"
    shell: """
echo hi
"""
    resources: mem_mb=10

rule format_flush_left:
    output: "b"
    shell: """
echo {x}
""".format(x=1)
    resources: mem_mb=10

rule docstring_flush_left:
    """
A rule.
"""
    output: "c"
    shell: "x"

rule bracket_flush_left:
    output: "d"
    params:
        p=[
1,
]
    shell: "x"

rule backslash:
    output: "e"
    resources: \
mem_mb=10
    shell: "x"
"#;
        let scan = scan(&[("Snakefile", snakefile)], "Snakefile", Some(V9_27));
        assert_eq!(
            memless(&scan),
            ["Snakefile:15 docstring_flush_left", "Snakefile:22 bracket_flush_left"]
        );
    }

    #[test]
    fn dynamic_resources_are_taken_as_sized_but_a_power_is_not() {
        let snakefile = r#"rule splat:
    output: "a"
    resources: **RES["big"]
    shell: "x"

rule func:
    output: "b"
    resources:
        mem_mb=lambda wc, input: max(2 * input.size_mb, 4000),
    shell: "x"

rule compared:
    output: "c"
    resources: runtime=10 if mem_mb == 1 else 2
    shell: "x"

rule power:
    output: "d"
    resources: runtime=2**4, disk_mb=2 ** 14
    shell: "x"

rule power_then_splat:
    output: "e"
    resources:
        disk_mb=2**14,
        **helper
    shell: "x"
"#;
        let scan = scan(&[("Snakefile", snakefile)], "Snakefile", Some(V9_27));
        assert_eq!(memless(&scan), ["Snakefile:12 compared", "Snakefile:17 power"]);
    }

    #[test]
    fn includes_are_followed_from_the_snakefile() {
        let snakefile = "include: \"rules/a.smk\"\ninclude: 'rules/missing.smk'\ninclude: f\"rules/{x}.smk\"\ninclude: \"rules/a.smk\"  # twice\n";
        // Relative to the including file, as Snakemake resolves them; a cycle is read once.
        let a = format!("include: \"../lib/b.smk\"\n{RULE_A}");
        let b = "include: \"../rules/a.smk\"\nrule b:\n    output: \"b\"\n    shell: \"x\"\n";
        let scan = scan(
            &[
                ("workflow/Snakefile", snakefile),
                ("workflow/rules/a.smk", &a),
                ("workflow/lib/b.smk", b),
                ("workflow/rules/unincluded.smk", RULE_A),
            ],
            "./workflow/Snakefile",
            Some(V9_27),
        );
        assert_eq!(memless(&scan), ["workflow/lib/b.smk:2 b", "workflow/rules/a.smk:2 a"]);
        assert_eq!(scan.profile_hint, "workflow/profiles/default/config.yaml");
        assert_eq!(include_literal("include: r'x.smk'  # c"), Some("x.smk".into()));
        assert_eq!(include_literal("include: config[\"x\"]"), None);
        assert_eq!(join_path("a/b/Snakefile", "../../../x"), None);
    }

    #[test]
    fn use_rule_inherits_its_base_and_module_rules_are_not_checked() {
        let snakefile = r#"module other:
    snakefile: "other/Snakefile"

use rule * from other as other_*

rule a:
    output: "a"
    shell: "x"

rule sized:
    output: "s"
    resources: mem_mb=100
    shell: "x"

use rule a as derived with:
    output: "b"

use rule a as derived_sized with:
    output: "c"
    resources: mem_mb=1

use rule sized as derived_from_sized with:
    output: "d"

use rule sized as derived_resized with:
    output: "e"
    resources: runtime=1

use rule unseen as derived_unseen with:
    output: "f"

use rule a, sized as copy_* with:
    output: "g"
"#;
        // set-resources sizes `a` by name only: a rule derived from it is not sized by it.
        let profile = "set-resources:\n  a:\n    mem_mb: 100\n";
        let scan = scan(
            &[
                ("Snakefile", snakefile),
                ("other/Snakefile", RULE_A),
                ("profiles/default/config.yaml", profile),
            ],
            "Snakefile",
            Some(V9_27),
        );
        assert_eq!(
            memless(&scan),
            ["Snakefile:15 derived", "Snakefile:25 derived_resized", "Snakefile:32 copy_a"]
        );
        assert_eq!(scan.unchecked_modules, ["other"]);
    }

    #[test]
    fn the_workflow_profile_is_read_as_snakemake_reads_it() {
        let list = "default-resources:\n  - mem_mb=30000  # per task\n  - runtime=60\n";
        let mapping = "default-resources:\n  mem_mb: 30000\n";
        let underscore = "default_resources:\n  mem_mb: 30000\n";
        let alias = "default-res: [mem_mb=1]\n";
        let alias_mapping = "default-res:\n  mem_mb: 1\n";
        let prefix = "default-resource: [mem_mb=1]\n";
        let ambiguous_prefix = "default-r: [mem_mb=1]\n";
        let string = "default-resources: \"runtime=1 mem_mb=2\"\n";
        let commented = "default-resources:\n  # - mem_mb=30000\n  - runtime=60\n";
        let no_mem = "jobs: 3\n";
        let unparseable = "default-resources: [\n";
        let repeated = "default-resources: [runtime=1]\ndefault-resources: [mem_mb=1]\n";
        let repeated_back = "default-resources: [mem_mb=1]\ndefault-resources: [runtime=1]\n";
        let gib = "default-resources: [mem_gib=8]\n";
        let mem_number = "default-resources: [mem=5000]\n";
        let mem_size = "default-resources: [mem=5G]\n";
        let p = "profiles/default/";
        let at = |v: (u32, u32, u32)| Some(EngineVersion::new(v.0, v.1, v.2));
        let v27 = Some(V9_27);
        #[rustfmt::skip]
        let cases: Vec<ProfileCase> = vec![
            (vec![("config.yaml", list)], v27, true),
            (vec![("config.yaml", mapping)], v27, true),
            (vec![("config.yaml", underscore)], v27, false),
            (vec![("config.yaml", alias)], v27, true),
            (vec![("config.yaml", alias_mapping)], v27, false),
            (vec![("config.yaml", prefix)], v27, true),
            (vec![("config.yaml", ambiguous_prefix)], v27, false),
            (vec![("config.yaml", string)], v27, true),
            (vec![("config.yaml", commented)], v27, false),
            (vec![("config.yaml", unparseable)], v27, false),
            (vec![("config.yaml", repeated)], v27, true),
            (vec![("config.yaml", repeated_back)], v27, false),
            (vec![("config.yaml", gib)], v27, false),
            // `.yml` is never read.
            (vec![("config.yml", list)], v27, false),
            // `profile.yaml` from 9.19 only; unknown release: some release does not read it.
            (vec![("profile.yaml", list)], v27, true),
            (vec![("profile.yaml", list)], Some(V9_0), false),
            (vec![("profile.yaml", list)], None, false),
            // One file: the highest eligible `.vN+`.
            (vec![("config.yaml", list), ("config.v9+.yaml", no_mem)], v27, false),
            (vec![("config.yaml", no_mem), ("profile.v8+.yaml", list)], v27, true),
            (vec![("profile.v10+.yaml", list)], v27, false),
            // A tie (directory order decides) covers only if every tied file does.
            (vec![("config.yaml", list), ("profile.yaml", no_mem)], v27, false),
            (vec![("config.yaml", list), ("profile.yaml", mapping)], v27, true),
            // Before 9.19 the name is matched from the start only.
            (vec![("config.yaml.bak", list)], at((9, 16, 2)), true),
            (vec![("config.yaml.bak", list)], v27, false),
            // A number for `mem` is megabytes from 9.20.0.
            (vec![("config.yaml", mem_number)], at((9, 21, 0)), true),
            (vec![("config.yaml", mem_number)], at((9, 17, 3)), false),
            (vec![("config.yaml", mem_size)], at((9, 0, 1)), true),
            (vec![("config.yaml", mem_number)], None, false),
            // Not the profile Snakemake loads by itself.
            (vec![("../awsbatch/config.yaml", list)], v27, false),
        ];
        for (profile_files, version, covered) in cases {
            let mut files = vec![("Snakefile", RULE_A)];
            let owned: Vec<String> = profile_files.iter().map(|(f, _)| format!("{p}{f}")).collect();
            for (path, (_, text)) in owned.iter().zip(&profile_files) {
                files.push((path.as_str(), text));
            }
            let scan = scan(&files, "Snakefile", version);
            let expected: &[&str] = if covered { &[] } else { &["Snakefile:1 a"] };
            assert_eq!(memless(&scan), expected, "{profile_files:?} under {version:?}");
        }
    }

    #[test]
    fn only_the_profile_beside_the_snakefile_is_loaded() {
        let profile = "default-resources: [mem_mb=1]\n";
        for (entrypoint, profile_path, covered) in [
            ("workflow/Snakefile", "workflow/profiles/default/config.yaml", true),
            ("workflow/rules/main.smk", "workflow/rules/profiles/default/config.yaml", true),
            // Snakemake runs from outside the source, so these are never loaded.
            ("workflow/rules/main.smk", "workflow/profiles/default/config.yaml", false),
            ("workflow/Snakefile", "profiles/default/config.yaml", false),
        ] {
            let scan =
                scan(&[(entrypoint, RULE_A), (profile_path, profile)], entrypoint, Some(V9_27));
            assert_eq!(scan.rules_without_mem.is_empty(), covered, "{entrypoint} + {profile_path}");
            if !covered {
                let dir = entrypoint.rsplit_once('/').map_or("", |(d, _)| d);
                assert_eq!(scan.profile_hint, format!("{dir}/profiles/default/config.yaml"));
            }
        }
    }

    #[test]
    fn the_profile_hint_is_the_newest_release_s_file() {
        // Only a file older releases read: with no release pinned, the hint is where the newest
        // one reads, not that file.
        let scan = scan(
            &[("Snakefile", RULE_A), ("profiles/default/config.yaml.bak", "jobs: 1\n")],
            "Snakefile",
            None,
        );
        assert_eq!(scan.profile_hint, "profiles/default/config.yaml");
    }

    #[test]
    fn an_unreadable_profile_sets_nothing_and_is_named() {
        let scan = scan(
            &[("Snakefile", RULE_A), ("profiles/default/config.yaml", "jobs: [\n")],
            "Snakefile",
            Some(V9_27),
        );
        assert_eq!(memless(&scan), ["Snakefile:1 a"]);
        assert_eq!(scan.unreadable_profile.as_deref(), Some("profiles/default/config.yaml"));
        assert_eq!(scan.profile_hint, "profiles/default/config.yaml");
    }

    #[test]
    fn set_resources_sizes_its_rules() {
        let snakefile = "rule a:\n    output: \"a\"\n    shell: \"x\"\n\nrule b:\n    output: \"b\"\n    shell: \"x\"\n\nrule c:\n    output: \"c\"\n    shell: \"x\"\n";
        for (profile, version, listed) in [
            (
                "set-resources:\n  a:\n    mem_mb: 100\n  b:\n    runtime: 5\n",
                V9_27,
                &["b", "c"][..],
            ),
            ("set-resources:\n  - a:mem_mb=100\n  - b:runtime=5\n", V9_27, &["b", "c"]),
            // An ambiguous abbreviation, and the underscore spelling, are refused.
            ("set-res:\n  - a:mem_mb=100\n", V9_27, &["a", "b", "c"]),
            ("set_resources:\n  - a:mem_mb=100\n", V9_27, &["a", "b", "c"]),
            ("set-resources:\n  - a:mem_mib=100\n", V9_27, &["b", "c"]),
            ("set-resources:\n  - a:mem_mib=100\n", V9_0, &["a", "b", "c"]),
        ] {
            let scan = scan(
                &[
                    ("workflow/Snakefile", snakefile),
                    ("workflow/profiles/default/config.yaml", profile),
                ],
                "workflow/Snakefile",
                Some(version),
            );
            let names: Vec<&str> = scan.rules_without_mem.iter().map(|r| r.rule.as_str()).collect();
            assert_eq!(names, listed, "{profile}");
            assert_eq!(scan.profile_hint, "workflow/profiles/default/config.yaml");
        }
    }

    #[test]
    fn a_missing_snakefile_lists_nothing() {
        let scan = scan(&[("rules/a.smk", RULE_A)], "Snakefile", Some(V9_27));
        assert!(scan.rules_without_mem.is_empty());
    }
}
