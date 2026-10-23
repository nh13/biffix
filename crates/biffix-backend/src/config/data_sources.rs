//! Data sources: the S3 locations a run's inputs can be picked from.
//!
//! The project bucket is always one. The others are registered in the deploy config — never at run
//! time — because each implies an IAM grant: CDK gives the run role, the coordinator and the
//! backend read on exactly those locations, so biffix cannot widen its own access. The backend
//! learns each project's list from its project context (`crate::config::projects`).
//!
//! Every path a caller names is relative to a source's root and is joined onto that root by string
//! concatenation, so a listing can never reach outside the source it names.

use serde::Deserialize;

/// The id of the project bucket's source.
pub(crate) const PROJECT_SOURCE: &str = "project";

/// Prefixes of the project bucket biffix keeps for itself: not shown at its root. `biffix/` is the
/// whole namespace biffix writes under (runs and the Snakemake cache alike), not just its runs.
const RESERVED_PREFIXES: [&str; 2] =
    [biffix_core::s3_layout::BIFFIX_ROOT, crate::aws::objects::SOURCE_PREFIX];

/// The part of [`RESERVED_PREFIXES`] the project source can browse: the runs' own folders.
const BROWSABLE_EXCEPTIONS: [&str; 1] = [biffix_core::s3_layout::RUNS_PREFIX];

/// What a data source holds, which decides how the UI offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataSourceKind {
    Project,
    Data,
    References,
}

impl DataSourceKind {
    fn as_str(self) -> &'static str {
        match self {
            DataSourceKind::Project => "project",
            DataSourceKind::Data => "data",
            DataSourceKind::References => "references",
        }
    }
}

/// One source: a bucket and a key prefix under it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct DataSource {
    pub id: String,
    pub name: String,
    pub bucket: String,
    /// Empty (the whole bucket) or ending in `/`.
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: DataSourceKind,
    /// Whether the deploy recorded that this source's bucket allows the browser's cross-origin
    /// Range reads (the alignment viewer's reference files). Only a hint for the viewer's message:
    /// the browser's own request is the test.
    #[serde(default)]
    pub viewer_cors: bool,
}

fn default_kind() -> DataSourceKind {
    DataSourceKind::Data
}

impl DataSource {
    /// The project bucket as a source.
    pub(crate) fn project(bucket: &str) -> Self {
        DataSource {
            id: PROJECT_SOURCE.into(),
            name: "Project data".into(),
            bucket: bucket.into(),
            prefix: String::new(),
            description: Some("This project's own bucket.".into()),
            kind: DataSourceKind::Project,
            // The project bucket's flag is `crate::config::projects::ProjectContext::viewer_cors`.
            viewer_cors: false,
        }
    }

    /// The source's wire view. `uploads` says whether the deploy enabled browser uploads to the
    /// project bucket (only the project bucket is ever writable).
    pub(crate) fn view(&self, uploads: bool) -> biffix_api::DataSource {
        biffix_api::DataSource {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            kind: self.kind.as_str().into(),
            uri: format!("s3://{}/{}", self.bucket, self.prefix),
            writable: uploads && self.kind == DataSourceKind::Project,
            hidden_prefixes: self
                .hidden_prefixes()
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            browsable_exceptions: self
                .browsable_exceptions()
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        }
    }

    /// The prefixes (relative to the root) [`Self::hides`] hides: the project bucket's
    /// biffix-owned ones; nothing for any other source.
    fn hidden_prefixes(&self) -> &'static [&'static str] {
        match self.kind {
            DataSourceKind::Project => &RESERVED_PREFIXES,
            DataSourceKind::Data | DataSourceKind::References => &[],
        }
    }

    /// The prefixes under a hidden one that [`Self::hides`] still lets through.
    fn browsable_exceptions(&self) -> &'static [&'static str] {
        match self.kind {
            DataSourceKind::Project => &BROWSABLE_EXCEPTIONS,
            DataSourceKind::Data | DataSourceKind::References => &[],
        }
    }

    /// The S3 key of `path` (relative to the source root).
    pub(crate) fn key_of(&self, path: &str) -> String {
        format!("{}{}", self.prefix, path.trim_start_matches('/'))
    }

    /// `path` relative to the source root for an S3 `key` under it; `None` outside it.
    pub(crate) fn path_of<'a>(&self, key: &'a str) -> Option<&'a str> {
        key.strip_prefix(self.prefix.as_str())
    }

    /// The `s3://` URI of a key in this source's bucket.
    pub(crate) fn uri_of(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    /// Whether `uri` is an object or folder this source holds: under its prefix, or its root. A
    /// folder may be named without its trailing `/` (`s3://b/refs` for the prefix `refs/`, or
    /// `s3://b` for a bucket's root), so the key is read as a folder too.
    pub(crate) fn holds(&self, uri: &str) -> bool {
        folder_or_object(uri)
            .is_some_and(|(bucket, key)| bucket == self.bucket && in_folder(key, &self.prefix))
    }

    /// Whether a folder or object at `path` (relative to the root) is hidden from browsing: the
    /// project bucket's biffix-owned prefixes, except the runs' own folders (`biffix/runs/` and
    /// below), so a run's results can be opened in the Data page. `biffix/` itself stays hidden,
    /// so the root (and a root search) never reaches into it. Served on the source's view
    /// (`hidden_prefixes`, `browsable_exceptions`), so the UI applies this same rule.
    ///
    /// Deliberately the WHOLE run folder, not only its registered outputs: its `inputs/`
    /// (`images.json`, samplesheets), the coordinator's `events.jsonl`, and Nextflow `work/` task
    /// directories (`.command.sh`/`.log`/`.err`, staged inputs) until the retention sweep removes
    /// them. Anyone who may browse data holds `ViewRuns`, the same capability that already lists
    /// and downloads every run's outputs (there is no per-run visibility), and an Expert user
    /// debugging a run needs its work directories and events. Nothing secret is written there:
    /// the run's ingest token travels only in the coordinator's environment.
    pub(crate) fn hides(&self, path: &str) -> bool {
        self.hidden_prefixes().iter().any(|p| path.starts_with(p))
            && !self.browsable_exceptions().iter().any(|p| path.starts_with(p))
    }

    /// Whether `path` is under a prefix biffix reserves for its own files (runs included), or is
    /// one of them named without its trailing `/` (`biffix`): never a reference, though a run's
    /// folder can be browsed ([`Self::hides`]).
    pub(crate) fn reserves(&self, path: &str) -> bool {
        self.hidden_prefixes().iter().any(|p| in_folder(path, p))
    }

    /// Whether a viewer may see the object at `uri` through this source: it [`holds`](Self::holds)
    /// it, it is an object (not a folder), and it is not under a prefix the source
    /// [`hides`](Self::hides). The one rule every endpoint naming an object by URI applies.
    pub(crate) fn shows(&self, uri: &str) -> bool {
        biffix_core::s3_layout::split_s3_uri(uri).is_some_and(|(bucket, key)| {
            bucket == self.bucket
                && !key.is_empty()
                && !key.ends_with('/')
                && self.path_of(key).is_some_and(|path| !self.hides(path))
        })
    }
}

/// The bucket and key of `uri` ([`biffix_core::s3_layout::split_s3_uri`]), a bucket's root named
/// without its trailing `/` (`s3://b`) included, with the empty key.
fn folder_or_object(uri: &str) -> Option<(&str, &str)> {
    biffix_core::s3_layout::split_s3_uri(uri).or_else(|| {
        uri.strip_prefix("s3://")
            .filter(|bucket| !bucket.is_empty() && !bucket.contains('/'))
            .map(|bucket| (bucket, ""))
    })
}

/// Parse a JSON list of data sources (a project context's `data_sources`). Prefixes are normalized
/// to end in `/`; a source with an empty id, the reserved `project` id, or a duplicate id is an
/// error (a deploy mistake, caught at start). For tests and fixtures: production reads the list as
/// part of a project context ([`validate_sources`]).
#[cfg(any(test, feature = "test-support"))]
pub fn parse_sources(json: &str) -> Result<Vec<DataSource>, DataSourceError> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let sources: Vec<DataSource> = serde_json::from_str(json)
        .map_err(|source| DataSourceError::Json { what: "data_sources".into(), source })?;
    validate_sources(sources, "data_sources")
}

/// A deploy-registered data source list the deploy got wrong (caught at start). `what` names where
/// the list came from.
#[derive(Debug, thiserror::Error)]
pub enum DataSourceError {
    /// Not a JSON list of data sources.
    #[error("{what}: {source}")]
    Json {
        what: String,
        #[source]
        source: serde_json::Error,
    },
    /// An empty id, the reserved `project` id, or an id used twice.
    #[error("{what}: bad or duplicate id `{id}`")]
    BadId { what: String, id: String },
    /// A source with no bucket.
    #[error("{what}: `{id}` has no bucket")]
    NoBucket { what: String, id: String },
}

/// Check and normalize deploy-registered sources (parsed from `what`, named in errors): the rules
/// of `parse_sources`, shared with a project context's `data_sources`
/// (`crate::config::projects`).
pub(crate) fn validate_sources(
    mut sources: Vec<DataSource>,
    what: &str,
) -> Result<Vec<DataSource>, DataSourceError> {
    let mut seen = std::collections::BTreeSet::new();
    for s in &mut sources {
        if s.id.is_empty() || s.id == PROJECT_SOURCE || !seen.insert(s.id.clone()) {
            return Err(DataSourceError::BadId { what: what.into(), id: s.id.clone() });
        }
        if s.bucket.is_empty() {
            return Err(DataSourceError::NoBucket { what: what.into(), id: s.id.clone() });
        }
        s.prefix = normalize_prefix(&s.prefix);
        if s.kind == DataSourceKind::Project {
            s.kind = DataSourceKind::Data;
        }
    }
    Ok(sources)
}

/// Whether `path` is under `prefix`, or is the folder `prefix` names without its trailing `/`
/// (what `format!("{path}/").starts_with(prefix)` says, without the allocation).
fn in_folder(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix) || prefix.strip_suffix('/') == Some(path)
}

/// Whether `path` (a relative key, no leading or trailing `/`) has an empty, `.` or `..` segment:
/// one that a tool normalizing the path could resolve somewhere else than the key names. The one
/// rule for the keys a caller writes under (uploads, an output prefix).
pub(crate) fn has_unplain_segment(path: &str) -> bool {
    path.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

/// `a/b` → `a/b/`; `/a/` → `a/`; `` → ``.
pub(crate) fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

/// Every source a viewer may browse: the project bucket first (when there is one), then the
/// registered ones in config order.
pub(crate) fn all_sources(project_bucket: &str, registered: &[DataSource]) -> Vec<DataSource> {
    let mut out = Vec::with_capacity(registered.len() + 1);
    if !project_bucket.is_empty() {
        out.push(DataSource::project(project_bucket));
    }
    out.extend(registered.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> DataSource {
        parse_sources(
            r#"[{"id":"core","name":"Sequencing core","bucket":"seq-core","prefix":"/deliveries"}]"#,
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn parses_and_normalizes() {
        let s = core();
        assert_eq!(s.prefix, "deliveries/");
        assert_eq!(s.kind, DataSourceKind::Data);
        assert_eq!(s.view(true).uri, "s3://seq-core/deliveries/");
        // A data source is never writable.
        assert!(!s.view(true).writable);
        assert!(parse_sources("").unwrap().is_empty());
    }

    #[test]
    fn the_deploy_s_viewer_cors_key_is_read() {
        // The key infra's DataSource.to_backend writes; absent means no rule recorded.
        let flagged = parse_sources(r#"[{"id":"r","name":"R","bucket":"b","viewer_cors":true}]"#);
        assert!(flagged.unwrap()[0].viewer_cors);
        assert!(!core().viewer_cors);
    }

    #[test]
    fn rejects_reserved_duplicate_and_bucketless_sources() {
        assert!(parse_sources(r#"[{"id":"project","name":"x","bucket":"b"}]"#).is_err());
        assert!(parse_sources(
            r#"[{"id":"a","name":"x","bucket":"b"},{"id":"a","name":"y","bucket":"c"}]"#
        )
        .is_err());
        assert!(parse_sources(r#"[{"id":"a","name":"x","bucket":""}]"#).is_err());
        assert!(parse_sources("{").is_err());
    }

    #[test]
    fn paths_stay_inside_the_source() {
        let s = core();
        assert_eq!(s.key_of("2026/batch-12/"), "deliveries/2026/batch-12/");
        assert_eq!(s.key_of("/2026/"), "deliveries/2026/");
        assert_eq!(s.path_of("deliveries/2026/a.fq.gz"), Some("2026/a.fq.gz"));
        assert_eq!(s.path_of("elsewhere/a.fq.gz"), None);
        assert!(s.holds("s3://seq-core/deliveries/2026/a.fq.gz"));
        assert!(!s.holds("s3://seq-core/private/a.fq.gz"));
        assert!(!s.holds("s3://other/deliveries/a.fq.gz"));
        // A folder named without its trailing slash: the source's own root, or one under it.
        assert!(s.holds("s3://seq-core/deliveries"));
        assert!(s.holds("s3://seq-core/deliveries/2026"));
        assert!(!s.holds("s3://seq-core/deliveries2"));
        assert!(!s.holds("s3://seq-core"));
        // A bucket's root, with or without its slash.
        let p = DataSource::project("proj");
        assert!(p.holds("s3://proj"));
        assert!(p.holds("s3://proj/"));
        assert!(!p.holds("s3://proj2"));
        assert!(!p.holds("proj/x"));
        assert!(!p.holds("s3:///x"));
    }

    #[test]
    fn shows_is_holds_minus_hidden_prefixes_and_folders() {
        let p = DataSource::project("proj");
        for (uri, shown) in [
            ("s3://proj/uploads/a.fq.gz", true),
            ("s3://proj/top.csv", true),
            ("s3://proj/workflow-sources/v1/source.tar.gz", false),
            // A run's results can be opened; the rest of biffix's namespace cannot.
            ("s3://proj/biffix/runs/r1/out.txt", true),
            ("s3://proj/biffix/cache/snakemake/x", false),
            ("s3://proj/biffix/other.txt", false),
            ("s3://proj/uploads/", false),
            ("s3://proj/", false),
            ("s3://other/uploads/a.fq.gz", false),
            ("proj/uploads/a.fq.gz", false),
        ] {
            assert_eq!(p.shows(uri), shown, "{uri}");
        }
        // Another source hides nothing under its prefix, and shows nothing outside it.
        let s = core();
        assert!(s.shows("s3://seq-core/deliveries/biffix/x.txt"));
        assert!(!s.shows("s3://seq-core/private/x.txt"));
    }

    #[test]
    fn the_project_source_hides_biffix_prefixes() {
        let p = DataSource::project("proj");
        // The root never shows `biffix/`, but a run's folder (and the runs folder) can be opened.
        assert!(p.hides("biffix/"));
        assert!(!p.hides("biffix/runs/"));
        assert!(!p.hides("biffix/runs/r1/results/a.txt"));
        assert!(p.reserves("biffix/runs/r1/results/a.txt"));
        // A reserved folder named without its trailing slash.
        assert!(p.reserves("biffix"));
        assert!(p.reserves("workflow-sources"));
        assert!(!p.reserves("biffixed"));
        assert!(!p.reserves("uploads/a.txt"));
        assert!(!core().reserves("biffix/runs/x"));
        assert!(p.hides("workflow-sources/x/source.tar.gz"));
        assert!(!p.hides("uploads/"));
        assert!(!core().hides("biffix/"));
        // The Snakemake cache is under `biffix/` too, not only runs.
        assert!(p.hides("biffix/cache/snakemake/x"));
        // The view carries the same rule, so the UI (and the mock, pinned to these literals in
        // ui/src/mock-contract.test.ts) hides exactly what the server hides.
        let view = p.view(false);
        assert_eq!(view.hidden_prefixes, ["biffix/", "workflow-sources/"]);
        assert_eq!(view.browsable_exceptions, ["biffix/runs/"]);
        let by_view = |path: &str| {
            view.hidden_prefixes.iter().any(|h| path.starts_with(h.as_str()))
                && !view.browsable_exceptions.iter().any(|e| path.starts_with(e.as_str()))
        };
        for path in [
            "",
            "biffix/",
            "biffix/cache/x",
            "biffix/runs/",
            "biffix/runs/r1/work/ab/cd/.command.sh",
            "workflow-sources/v1/source.tar.gz",
            "uploads/a.fq",
            "biffixed/x",
        ] {
            assert_eq!(by_view(path), p.hides(path), "{path}");
        }
        let core_view = core().view(false);
        assert!(core_view.hidden_prefixes.is_empty() && core_view.browsable_exceptions.is_empty());
        assert_eq!(serde_json::to_value(&core_view).unwrap().get("hidden_prefixes"), None);
        // The project bucket is writable only when the deploy enabled uploads.
        assert!(p.view(true).writable);
        assert!(!p.view(false).writable);
        let all = all_sources("proj", &[core()]);
        assert_eq!(all.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["project", "core"]);
        assert_eq!(all_sources("", &[core()]).len(), 1);
    }
}
