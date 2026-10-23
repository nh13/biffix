//! Per-project context: what a run, a registration or a data listing needs to know about the
//! project it is in (its bucket, run role, coordinator template, task log group, data sources, and
//! whether browsers may upload into or read from its bucket).
//!
//! A deploy declares one or more projects. Each has its own bucket, queues, roles and KMS key, so
//! a project is the blast radius of anything run in it, and every request picks its project's
//! context instead of a deploy-wide one:
//!
//! - a submit, a launch, a registration: the workflow version's project
//!   ([`Projects::for_version`]);
//! - a reader of a run (outputs, reports, QC, logs, provenance, resume): the project the run
//!   actually ran in ([`Projects::for_run`]), the one rule every reader goes through: the project
//!   whose bucket holds the run's recorded output prefix.
//!
//! The deploy writes one SSM parameter per project (`<BIFFIX_PROJECTS_PATH><id>`, a JSON
//! [`ProjectContext`]) and names the default (`BIFFIX_DEFAULT_PROJECT`); [`ProjectsHandle`] keeps
//! them in process and refreshes them every [`REFRESH_AFTER`]. Outside a deploy (the `server` bin,
//! local development), `BIFFIX_PROJECTS_FILE` names a JSON file of the same contexts instead
//! ([`FileProjectsLoader`]).

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;

use crate::config::data_sources::{all_sources, validate_sources, DataSource, DataSourceError};
use crate::store::RunDetailView;

/// How long a loaded set of project contexts is used before it is read again.
#[cfg(any(test, feature = "aws"))]
pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(5 * 60);

/// How long after a failed refresh the next is tried (the last good contexts serve meanwhile).
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30);

/// How long a refresh may take before the request that runs it gives up on it and carries on
/// with the current contexts (an SSM brownout must not stall a request for the SDK's whole retry
/// budget).
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How soon after a read a request naming an unknown project may force another
/// ([`ProjectsHandle::current_for`]): bounds the SSM calls a stream of requests naming a project
/// the deploy truly lacks can cause.
const FORCED_READ_GAP: Duration = Duration::from_secs(10);

/// One project's context, as the deploy writes it (one SSM parameter per project, JSON). Keys the
/// backend does not know are ignored, so the deploy can add some before the backend reads them.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ProjectContext {
    /// The project id: a workflow version's `project`, the `biffix:project` tag of its runs' jobs.
    pub id: String,
    /// The project's S3 bucket: runs write under it, sources are staged into it.
    pub bucket: String,
    /// The project's run base role: Batch runs the coordinator's children as it.
    pub run_role_arn: String,
    /// The project's coordinator job definition (revision ARN): the template its coordinator
    /// variants are cloned from (its job role is the project's coordinator role).
    pub coordinator_job_definition: String,
    /// Where the project's task jobs log. Empty only in a composition with no deploy behind it
    /// (the tests'), whose task streams are all in [`crate::aws::logs::BATCH_LOG_GROUP`].
    pub task_log_group: String,
    /// Whether browsers may upload into the project bucket (it has the upload CORS rule).
    #[serde(default)]
    pub uploads: bool,
    /// Whether the project bucket allows the alignment viewer's cross-origin Range reads.
    #[serde(default)]
    pub viewer_cors: bool,
    /// The S3 locations besides the project bucket the project's file picker may browse.
    #[serde(default)]
    pub data_sources: Vec<DataSource>,
}

impl ProjectContext {
    /// Parse and check one project's context (an SSM parameter's value). `what` names it in errors.
    #[cfg(any(test, feature = "aws"))]
    pub(crate) fn from_json(json: &str, what: &str) -> Result<Self, ProjectsError> {
        let ctx: ProjectContext = serde_json::from_str(json)
            .map_err(|source| ProjectsError::Json { what: what.into(), source })?;
        ctx.checked(what)
    }

    /// Normalize (source prefixes end in `/`) and check the context.
    fn checked(mut self, what: &str) -> Result<Self, ProjectsError> {
        let missing = |field: &'static str| ProjectsError::MissingField {
            what: what.into(),
            id: self.id.clone(),
            field,
        };
        if self.id.is_empty() {
            return Err(ProjectsError::NoId { what: what.into() });
        }
        if self.bucket.is_empty() {
            return Err(missing("bucket"));
        }
        if self.task_log_group.is_empty() {
            return Err(missing("task_log_group"));
        }
        // Present but empty: every launch in the project would fail at SubmitJob, far from here.
        if self.run_role_arn.is_empty() {
            return Err(missing("run_role_arn"));
        }
        if self.coordinator_job_definition.is_empty() {
            return Err(missing("coordinator_job_definition"));
        }
        self.data_sources = validate_sources(
            std::mem::take(&mut self.data_sources),
            &format!("{what}: data_sources"),
        )?;
        Ok(self)
    }

    /// Every source the project's viewers may browse: its bucket first, then its data sources.
    pub(crate) fn sources(&self) -> Vec<DataSource> {
        all_sources(&self.bucket, &self.data_sources)
    }

    /// Whether one of [`Self::sources`] holds `uri` ([`DataSource::holds`]), without building them.
    pub(crate) fn holds(&self, uri: &str) -> bool {
        (!self.bucket.is_empty() && DataSource::project(&self.bucket).holds(uri))
            || self.data_sources.iter().any(|s| s.holds(uri))
    }

    /// The one of [`Self::sources`] with id `id` (the project bucket's is
    /// [`crate::config::data_sources::PROJECT_SOURCE`]), cloning only it.
    pub(crate) fn find_source(&self, id: &str) -> Option<DataSource> {
        if id == crate::config::data_sources::PROJECT_SOURCE {
            return (!self.bucket.is_empty()).then(|| DataSource::project(&self.bucket));
        }
        self.data_sources.iter().find(|s| s.id == id).cloned()
    }

    /// What a coordinator launch needs of the project.
    pub(crate) fn launch_project(&self) -> crate::aws::batch::LaunchProject {
        crate::aws::batch::LaunchProject {
            id: self.id.clone(),
            bucket: self.bucket.clone(),
            run_role_arn: self.run_role_arn.clone(),
        }
    }
}

/// The deploy's project contexts are not usable: a context the deploy wrote wrong, a set of them
/// that does not fit together, or a read that failed. `what` names where a context came from (an
/// SSM parameter, a file and its index).
#[derive(Debug, thiserror::Error)]
pub enum ProjectsError {
    /// Not a JSON project context (or list of them).
    #[error("{what}: {source}")]
    Json {
        what: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{what}: the project has no id")]
    NoId { what: String },
    #[error("{what}: project `{id}` has no {field}")]
    MissingField { what: String, id: String, field: &'static str },
    #[error(transparent)]
    DataSources(#[from] DataSourceError),
    /// Two projects share a bucket (a run's project is told by its bucket).
    #[error("projects `{first}` and `{second}` share the bucket {bucket}")]
    SharedBucket { first: String, second: String, bucket: String },
    #[error("project `{id}` is configured twice")]
    Repeated { id: String },
    #[error("no project is configured")]
    NoProject,
    #[error("{what}: no project context")]
    Empty { what: String },
    #[error("the default project `{default}` is not configured (projects: {})", configured.join(", "))]
    UnknownDefault { default: String, configured: Vec<String> },
    /// An SSM parameter that holds a project other than the one it is named for.
    #[error("{name}: holds project `{id}`, but is not named {path}{id}")]
    Misnamed { name: String, id: String, path: String },
    #[error("no project context under {path}")]
    NoneUnder { path: String },
    /// The projects file could not be read.
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    /// The deploy's store of contexts (SSM) could not be read.
    #[error("reading the project contexts under {path}: {reason}")]
    Read { path: String, reason: String },
    /// A refresh took longer than it may.
    #[error("no answer within {0:?}")]
    TimedOut(Duration),
}

/// A workflow version names a project this deploy does not configure (on a deploy with more than
/// one project; a single-project deploy runs every version in its one project).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "project `{project}` is not configured on this deploy (projects: {})",
    configured.join(", ")
)]
pub(crate) struct UnknownProject {
    pub(crate) project: String,
    pub(crate) configured: Vec<String>,
}

/// The deploy's projects, by id, and the default one. Never empty.
#[derive(Clone, Debug, PartialEq)]
pub struct Projects {
    by_id: BTreeMap<String, ProjectContext>,
    default_id: String,
}

impl Default for Projects {
    /// One project with nothing configured (no bucket): a local composition, and the tests'.
    fn default() -> Self {
        Self::single(ProjectContext::default())
    }
}

impl Projects {
    /// A deploy of one project.
    pub(crate) fn single(ctx: ProjectContext) -> Self {
        let default_id = ctx.id.clone();
        Self { by_id: BTreeMap::from([(default_id.clone(), ctx)]), default_id }
    }

    /// The deploy's projects with `default_id` the default. Refuses none, a repeated id, two
    /// projects sharing a bucket (a run's project is told by its bucket), and an unknown default.
    pub fn new(contexts: Vec<ProjectContext>, default_id: &str) -> Result<Self, ProjectsError> {
        let mut by_id = BTreeMap::new();
        let mut buckets = BTreeMap::new();
        for ctx in contexts {
            // The repeated id first: the same context twice is that, not two sharing a bucket.
            if by_id.contains_key(&ctx.id) {
                return Err(ProjectsError::Repeated { id: ctx.id });
            }
            if let Some(other) = buckets.insert(ctx.bucket.clone(), ctx.id.clone()) {
                return Err(ProjectsError::SharedBucket {
                    first: other,
                    second: ctx.id,
                    bucket: ctx.bucket,
                });
            }
            by_id.insert(ctx.id.clone(), ctx);
        }
        if by_id.is_empty() {
            return Err(ProjectsError::NoProject);
        }
        if !by_id.contains_key(default_id) {
            return Err(ProjectsError::UnknownDefault {
                default: default_id.to_string(),
                configured: by_id.keys().cloned().collect(),
            });
        }
        Ok(Self { by_id, default_id: default_id.to_string() })
    }

    pub(crate) fn get(&self, id: &str) -> Option<&ProjectContext> {
        self.by_id.get(id)
    }

    /// The project whose bucket is `bucket`.
    pub(crate) fn by_bucket(&self, bucket: &str) -> Option<&ProjectContext> {
        self.by_id.values().find(|p| p.bucket == bucket)
    }

    /// The default project.
    pub(crate) fn default_project(&self) -> &ProjectContext {
        &self.by_id[&self.default_id]
    }

    /// Every project, by id.
    pub fn iter(&self) -> impl Iterator<Item = &ProjectContext> {
        self.by_id.values()
    }

    /// The configured project ids, sorted.
    pub(crate) fn ids(&self) -> Vec<String> {
        self.by_id.keys().cloned().collect()
    }

    /// Whether the deploy has more than one project.
    pub(crate) fn is_multi(&self) -> bool {
        self.by_id.len() > 1
    }

    /// The project a workflow version labelled `project` runs in. A single-project deploy runs
    /// every version in its one project whatever the label; a deploy of several refuses a label it
    /// does not configure, rather than run the version in a project it did not ask for.
    pub(crate) fn for_version(&self, project: &str) -> Result<&ProjectContext, UnknownProject> {
        if !self.is_multi() {
            return Ok(self.default_project());
        }
        self.get(project)
            .ok_or_else(|| UnknownProject { project: project.to_string(), configured: self.ids() })
    }

    /// The project a run ran in, from its recorded output prefix and its version's project: the
    /// one rule for every reader of a run (see [`Self::for_run_parts`]).
    pub(crate) fn for_run(&self, run: &RunDetailView) -> &ProjectContext {
        self.for_run_parts(run.output_prefix.as_deref(), run.run.project.as_deref())
    }

    /// [`Self::for_run`] from a run's recorded `output_prefix` and its version's `project`: the
    /// project whose bucket the prefix is in, else (a bucket no configured project owns, e.g. a
    /// project since removed) the version's project, else the default. No recorded prefix (the
    /// run event's field is optional): the default project.
    pub(crate) fn for_run_parts(
        &self,
        output_prefix: Option<&str>,
        project: Option<&str>,
    ) -> &ProjectContext {
        let Some(prefix) = output_prefix else {
            return self.default_project();
        };
        biffix_core::s3_layout::split_s3_uri(prefix)
            .and_then(|(bucket, _)| self.by_bucket(bucket))
            .or_else(|| project.and_then(|p| self.get(p)))
            .unwrap_or_else(|| self.default_project())
    }

    /// The other projects whose runs can read `uri` when a run of `project` cannot: `uri` is in
    /// another project's bucket, or in a data source only other projects have (each project's
    /// roles reach only its own bucket and data sources). Empty when a run of `project` can read
    /// it (its bucket or one of its data sources holds it), and when no configured location holds
    /// it (a public or externally granted bucket: whether it exists is the existence check's call).
    pub(crate) fn readable_only_by(&self, project: &ProjectContext, uri: &str) -> Vec<&str> {
        let holds = |p: &ProjectContext| p.holds(uri);
        if holds(project) {
            return Vec::new();
        }
        self.by_id
            .values()
            .filter(|p| p.id != project.id && holds(p))
            .map(|p| p.id.as_str())
            .collect()
    }

    /// Every source of every project: what a deploy-wide object (a reference bundle) may name.
    pub(crate) fn all_sources(&self) -> Vec<DataSource> {
        self.by_id.values().flat_map(ProjectContext::sources).collect()
    }

    /// Whether the deploy recorded a viewer CORS rule on `bucket`: a project bucket's own flag,
    /// else any data source in that bucket that has one (the rule is the bucket's, so one source's
    /// flag speaks for every source in it).
    pub(crate) fn bucket_allows_viewer(&self, bucket: &str) -> bool {
        match self.by_bucket(bucket) {
            Some(project) => project.viewer_cors,
            None => self
                .by_id
                .values()
                .flat_map(|p| &p.data_sources)
                .any(|s| s.bucket == bucket && s.viewer_cors),
        }
    }
}

/// Reads the deploy's projects (from SSM in production).
#[async_trait]
pub trait ProjectsLoader: Send + Sync {
    async fn load(&self) -> Result<Projects, ProjectsError>;
}

struct Loaded {
    projects: Arc<Projects>,
    /// When to read them again; `None` never.
    refresh_at: Option<Instant>,
    /// When a request last forced a read ([`ProjectsHandle::current_for`]).
    forced_at: Option<Instant>,
}

/// The deploy's projects as this process knows them: fixed (the tests) or loaded and refreshed
/// every [`REFRESH_AFTER`], keeping the last good set when a refresh fails. A project added by a
/// deploy is seen within that bound, or at once by a request that names it
/// ([`Self::current_for`]); nothing a deploy removes is in use by a request for long.
pub struct ProjectsHandle {
    loaded: RwLock<Loaded>,
    loader: Option<Arc<dyn ProjectsLoader>>,
    ttl: Duration,
    /// One refresh at a time.
    refreshing: tokio::sync::Mutex<()>,
}

impl Default for ProjectsHandle {
    fn default() -> Self {
        Self::fixed(Projects::default())
    }
}

impl ProjectsHandle {
    /// Projects that never change.
    pub(crate) fn fixed(projects: Projects) -> Self {
        Self {
            loaded: RwLock::new(Loaded {
                projects: Arc::new(projects),
                refresh_at: None,
                forced_at: None,
            }),
            loader: None,
            ttl: Duration::MAX,
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    /// Projects read through `loader` now (a failure is the caller's startup error) and again
    /// once `ttl` has passed.
    pub async fn load(
        loader: Arc<dyn ProjectsLoader>,
        ttl: Duration,
    ) -> Result<Self, ProjectsError> {
        let projects = loader.load().await?;
        Ok(Self {
            loaded: RwLock::new(Loaded {
                projects: Arc::new(projects),
                refresh_at: Instant::now().checked_add(ttl),
                forced_at: None,
            }),
            loader: Some(loader),
            ttl,
            refreshing: tokio::sync::Mutex::new(()),
        })
    }

    fn snapshot(&self) -> (Arc<Projects>, bool, Option<Instant>) {
        let loaded = self.loaded.read().expect("projects lock");
        let due = loaded.refresh_at.is_some_and(|at| Instant::now() >= at);
        (loaded.projects.clone(), due, loaded.forced_at)
    }

    /// The current projects, read again first when they are due. While another request is
    /// reading them, the current set serves.
    pub(crate) async fn current(&self) -> Arc<Projects> {
        let (projects, due, _) = self.snapshot();
        if !due || self.loader.is_none() {
            return projects;
        }
        let Ok(guard) = self.refreshing.try_lock() else {
            return projects;
        };
        self.refresh(guard, false).await
    }

    /// The current projects, read again first (forced reads at most once per
    /// [`FORCED_READ_GAP`], however many requests ask) when they do not configure `project`: a
    /// request naming a project a deploy just added need not wait out [`REFRESH_AFTER`]. A
    /// single-project set is read again too, though [`Projects::for_version`] would route any label
    /// to its one project: the label may name a project the deploy has just added beside it.
    /// `None` (no project named) is [`Self::current`].
    pub(crate) async fn current_for(&self, project: Option<&str>) -> Arc<Projects> {
        let projects = self.current().await;
        let Some(id) = project.map(str::trim).filter(|p| !p.is_empty()) else {
            return projects;
        };
        if self.loader.is_none() || projects.get(id).is_some() {
            return projects;
        }
        let guard = self.refreshing.lock().await;
        self.refresh(guard, true).await
    }

    /// Read the projects again, holding the refresh lock: unless, now the lock is held, they are
    /// no longer due (another request read them meanwhile) or, `forced`, another forced read was
    /// within [`FORCED_READ_GAP`]. A read that fails or takes longer than [`READ_TIMEOUT`] keeps
    /// the current set and is retried after [`RETRY_AFTER_FAILURE`].
    async fn refresh(
        &self,
        _guard: tokio::sync::MutexGuard<'_, ()>,
        forced: bool,
    ) -> Arc<Projects> {
        let (projects, due, forced_at) = self.snapshot();
        let Some(loader) = self.loader.as_ref() else {
            return projects;
        };
        let skip =
            if forced { forced_at.is_some_and(|at| at.elapsed() < FORCED_READ_GAP) } else { !due };
        if skip {
            return projects;
        }
        let read = tokio::time::timeout(READ_TIMEOUT, loader.load())
            .await
            .unwrap_or(Err(ProjectsError::TimedOut(READ_TIMEOUT)));
        let now = Instant::now();
        let mut loaded = self.loaded.write().expect("projects lock");
        if forced {
            loaded.forced_at = Some(now);
        }
        match read {
            Ok(fresh) => {
                loaded.projects = Arc::new(fresh);
                loaded.refresh_at = now.checked_add(self.ttl);
            }
            Err(e) => {
                tracing::warn!(error = %e, "projects: refreshing the project contexts failed; keeping the last good ones");
                loaded.refresh_at = now.checked_add(RETRY_AFTER_FAILURE.min(self.ttl));
            }
        }
        loaded.projects.clone()
    }
}

/// Reads the projects from a JSON file (`BIFFIX_PROJECTS_FILE`): one [`ProjectContext`] (an SSM
/// parameter's value) or an array of them. The default is `default_id` (`BIFFIX_DEFAULT_PROJECT`)
/// when given, else the file's first project. Read again on every refresh, so an edit is picked up.
pub(crate) struct FileProjectsLoader {
    path: std::path::PathBuf,
    default_id: Option<String>,
}

impl FileProjectsLoader {
    pub(crate) fn new(path: impl Into<std::path::PathBuf>, default_id: Option<String>) -> Self {
        Self { path: path.into(), default_id }
    }
}

#[async_trait]
impl ProjectsLoader for FileProjectsLoader {
    async fn load(&self) -> Result<Projects, ProjectsError> {
        let what = self.path.display().to_string();
        let json = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|source| ProjectsError::Io { what: what.clone(), source })?;
        parse_projects_file(&json, &what, self.default_id.as_deref())
    }
}

/// The projects a [`FileProjectsLoader`] file holds (`what` names it in errors).
pub(crate) fn parse_projects_file(
    json: &str,
    what: &str,
    default_id: Option<&str>,
) -> Result<Projects, ProjectsError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|source| ProjectsError::Json { what: what.into(), source })?;
    let values = match value {
        serde_json::Value::Array(values) => values,
        one => vec![one],
    };
    let contexts = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            let at = format!("{what}[{i}]");
            serde_json::from_value::<ProjectContext>(v)
                .map_err(|source| ProjectsError::Json { what: at.clone(), source })?
                .checked(&at)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let default_id = match default_id {
        Some(id) => id.to_string(),
        None => contexts
            .first()
            .map(|c| c.id.clone())
            .ok_or_else(|| ProjectsError::Empty { what: what.into() })?,
    };
    Projects::new(contexts, &default_id)
}

/// The projects SSM holds under `path` (ending in `/`) as `(name, value)` parameters, `default_id`
/// the default: each parameter is named for the project its value describes.
#[cfg(any(test, feature = "aws"))]
pub(crate) fn parse_parameters(
    path: &str,
    params: &[(String, String)],
    default_id: &str,
) -> Result<Projects, ProjectsError> {
    let contexts = params
        .iter()
        .map(|(name, value)| {
            let ctx = ProjectContext::from_json(value, name)?;
            match name.strip_prefix(path) {
                Some(id) if id == ctx.id => Ok(ctx),
                _ => Err(ProjectsError::Misnamed {
                    name: name.clone(),
                    id: ctx.id,
                    path: path.into(),
                }),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if contexts.is_empty() {
        return Err(ProjectsError::NoneUnder { path: path.into() });
    }
    Projects::new(contexts, default_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ctx(id: &str, bucket: &str) -> ProjectContext {
        ProjectContext {
            id: id.into(),
            bucket: bucket.into(),
            run_role_arn: format!("arn:aws:iam::1:role/{id}-run"),
            coordinator_job_definition: format!("arn:aws:batch:r:1:job-definition/{id}-coord:1"),
            ..Default::default()
        }
    }

    fn two() -> Projects {
        Projects::new(vec![ctx("a", "bucket-a"), ctx("b", "bucket-b")], "a").unwrap()
    }

    #[test]
    fn a_context_parses_with_unknown_keys_and_normalizes() {
        let json = r#"{"id":"b","bucket":"bucket-b","run_role_arn":"arn:role",
            "coordinator_job_definition":"arn:jd","task_log_group":"/g","kms_key_arn":null,
            "uploads":true,"viewer_cors":false,"future_key":{"x":1},
            "data_sources":[{"id":"core","name":"Core","bucket":"seq","prefix":"/d","viewer_cors":true}]}"#;
        let c = ProjectContext::from_json(json, "p").unwrap();
        assert_eq!(c.id, "b");
        assert_eq!(c.task_log_group, "/g");
        assert!(c.uploads && !c.viewer_cors);
        assert_eq!(c.data_sources[0].prefix, "d/");
        assert!(c.data_sources[0].viewer_cors);
        assert_eq!(
            c.sources().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["project", "core"]
        );
    }

    #[test]
    fn a_context_missing_a_required_key_or_with_a_bad_source_is_refused() {
        for json in [
            r#"{"bucket":"b","run_role_arn":"r","coordinator_job_definition":"j","task_log_group":"g"}"#,
            r#"{"id":"","bucket":"b","run_role_arn":"r","coordinator_job_definition":"j","task_log_group":"g"}"#,
            r#"{"id":"a","bucket":"","run_role_arn":"r","coordinator_job_definition":"j","task_log_group":"g"}"#,
            r#"{"id":"a","bucket":"b","coordinator_job_definition":"j","task_log_group":"g"}"#,
            r#"{"id":"a","bucket":"b","run_role_arn":"r","coordinator_job_definition":"j"}"#,
            r#"{"id":"a","bucket":"b","run_role_arn":"r","coordinator_job_definition":"j","task_log_group":""}"#,
            r#"{"id":"a","bucket":"b","run_role_arn":"","coordinator_job_definition":"j","task_log_group":"g"}"#,
            r#"{"id":"a","bucket":"b","run_role_arn":"r","coordinator_job_definition":"","task_log_group":"g"}"#,
            r#"{"id":"a","bucket":"b","run_role_arn":"r","coordinator_job_definition":"j","task_log_group":"g",
                "data_sources":[{"id":"project","name":"x","bucket":"c"}]}"#,
        ] {
            assert!(ProjectContext::from_json(json, "p").is_err(), "{json}");
        }
    }

    #[test]
    fn projects_refuse_duplicates_shared_buckets_and_an_unknown_default() {
        assert!(Projects::new(vec![ctx("a", "x"), ctx("a", "y")], "a").is_err());
        // The same context twice is a repeated id, not two projects sharing a bucket.
        assert!(matches!(
            Projects::new(vec![ctx("a", "x"), ctx("a", "x")], "a"),
            Err(ProjectsError::Repeated { id }) if id == "a"
        ));
        assert!(Projects::new(vec![ctx("a", "x"), ctx("b", "x")], "a").is_err());
        assert!(Projects::new(vec![ctx("a", "x")], "b").is_err());
        assert!(Projects::new(vec![], "a").is_err());
    }

    #[test]
    fn a_projects_file_holds_one_context_or_several() {
        let value = |id: &str, bucket: &str| {
            format!(
                r#"{{"id":"{id}","bucket":"{bucket}","run_role_arn":"r",
                    "coordinator_job_definition":"j","task_log_group":"/biffix/{id}/batch"}}"#
            )
        };
        // One context (the golden SSM value is one): a single-project deploy.
        let golden = include_str!("../../tests/fixtures/project_context.json");
        let p = parse_projects_file(golden, "f", None).unwrap();
        assert_eq!(p.ids(), ["imported"]);
        assert!(!p.is_multi());
        // Several: the first is the default unless one is named.
        let both = format!("[{},{}]", value("b", "y"), value("a", "x"));
        assert_eq!(parse_projects_file(&both, "f", None).unwrap().default_project().id, "b");
        assert_eq!(parse_projects_file(&both, "f", Some("a")).unwrap().default_project().id, "a");
        // A bad context names its place in the file; an empty file has no default.
        let err = parse_projects_file(&format!("[{},{{}}]", value("a", "x")), "f", None);
        assert!(err.unwrap_err().to_string().starts_with("f[1]"));
        assert!(parse_projects_file("[]", "f", None).is_err());
        assert!(parse_projects_file(&both, "f", Some("c")).is_err());
    }

    #[tokio::test]
    async fn a_projects_file_is_read_through_the_loader() {
        let dir = std::env::temp_dir().join(format!("biffix-projects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projects.json");
        std::fs::write(&path, include_str!("../../tests/fixtures/project_context.json")).unwrap();
        let loaded = FileProjectsLoader::new(&path, None).load().await.unwrap();
        assert_eq!(loaded.default_project().bucket, "acme-imported");
        let missing = FileProjectsLoader::new(dir.join("nope.json"), None).load().await;
        assert!(missing.unwrap_err().to_string().contains("nope.json"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_single_project_deploy_runs_every_version_in_its_project() {
        let p = Projects::single(ctx("a", "bucket-a"));
        assert_eq!(p.for_version("a").unwrap().id, "a");
        assert_eq!(p.for_version("my-workflow").unwrap().id, "a");
    }

    #[test]
    fn a_multi_project_deploy_refuses_an_unknown_version_project() {
        let p = two();
        assert_eq!(p.for_version("b").unwrap().bucket, "bucket-b");
        let err = p.for_version("c").unwrap_err();
        assert_eq!(
            err.to_string(),
            "project `c` is not configured on this deploy (projects: a, b)"
        );
    }

    #[test]
    fn a_run_is_in_its_prefix_s_project_then_its_version_s_then_the_default() {
        let p = two();
        // The recorded prefix's bucket wins, even over the version's label (a run launched before
        // routing, labelled b, ran in a's bucket).
        assert_eq!(p.for_run_parts(Some("s3://bucket-a/x/"), Some("b")).id, "a");
        assert_eq!(p.for_run_parts(Some("s3://bucket-b/x/"), Some("a")).id, "b");
        // A prefix in no project's bucket: the version's project, else the default.
        assert_eq!(p.for_run_parts(Some("s3://elsewhere/x/"), Some("b")).id, "b");
        assert_eq!(p.for_run_parts(Some("s3://elsewhere/x/"), Some("unknown")).id, "a");
        // No recorded prefix: the default project, whatever its label says.
        assert_eq!(p.for_run_parts(None, Some("b")).id, "a");
        assert_eq!(p.for_run_parts(None, Some("unknown")).id, "a");
        assert_eq!(p.for_run_parts(None, None).id, "a");
    }

    #[test]
    fn a_location_only_other_projects_hold_is_named_by_them() {
        let mut b = ctx("b", "bucket-b");
        b.data_sources = validate_sources(
            serde_json::from_str(
                r#"[{"id":"core","name":"C","bucket":"seq","prefix":"b-only/"},
                    {"id":"shared","name":"S","bucket":"shared"}]"#,
            )
            .unwrap(),
            "t",
        )
        .unwrap();
        let mut a = ctx("a", "bucket-a");
        a.data_sources = validate_sources(
            serde_json::from_str(r#"[{"id":"shared","name":"S","bucket":"shared"}]"#).unwrap(),
            "t",
        )
        .unwrap();
        let p = Projects::new(vec![a, b], "a").unwrap();
        let (a, b) = (p.get("a").unwrap(), p.get("b").unwrap());
        // Its own bucket (any key, a folder too) and its own sources: readable.
        assert!(p.readable_only_by(a, "s3://bucket-a/x/y.fq").is_empty());
        assert!(p.readable_only_by(a, "s3://bucket-a/").is_empty());
        assert!(p.readable_only_by(a, "s3://shared/ref.fa").is_empty());
        assert!(p.readable_only_by(b, "s3://seq/b-only/r.fq").is_empty());
        // Another project's bucket or a source only it has: that project's.
        assert_eq!(p.readable_only_by(a, "s3://bucket-b/in.fq"), ["b"]);
        assert_eq!(p.readable_only_by(a, "s3://seq/b-only/r.fq"), ["b"]);
        assert_eq!(p.readable_only_by(b, "s3://bucket-a/biffix/runs/r/out.bam"), ["a"]);
        // Outside every configured location (the same bucket outside a source's prefix too):
        // the existence check's call.
        assert!(p.readable_only_by(a, "s3://public-refs/hg38.fa").is_empty());
        assert!(p.readable_only_by(a, "s3://seq/elsewhere/r.fq").is_empty());
        // A folder named without its trailing slash is the same folder: another project's source
        // root, a folder under it, or another project's bucket root.
        assert_eq!(p.readable_only_by(a, "s3://seq/b-only"), ["b"]);
        assert_eq!(p.readable_only_by(a, "s3://seq/b-only/run1"), ["b"]);
        assert_eq!(p.readable_only_by(a, "s3://bucket-b"), ["b"]);
        assert!(p.readable_only_by(a, "s3://bucket-a").is_empty());
        assert!(p.readable_only_by(a, "s3://shared").is_empty());
    }

    #[test]
    fn the_viewer_flag_is_the_project_bucket_s_else_any_source_s() {
        let mut a = ctx("a", "bucket-a");
        a.viewer_cors = true;
        let mut b = ctx("b", "bucket-b");
        b.data_sources = validate_sources(
            serde_json::from_str(r#"[{"id":"r","name":"R","bucket":"refs","viewer_cors":true}]"#)
                .unwrap(),
            "t",
        )
        .unwrap();
        let p = Projects::new(vec![a, b], "a").unwrap();
        assert!(p.bucket_allows_viewer("bucket-a"));
        assert!(!p.bucket_allows_viewer("bucket-b"));
        assert!(p.bucket_allows_viewer("refs"));
        assert!(!p.bucket_allows_viewer("other"));
        // Every project's sources, each project's bucket among them.
        let buckets: Vec<String> = p.all_sources().into_iter().map(|s| s.bucket).collect();
        assert_eq!(buckets, ["bucket-a", "bucket-b", "refs"]);
    }

    #[test]
    fn ssm_parameters_are_named_for_their_project() {
        let path = "/biffix/t/projects/";
        let value = |id: &str, bucket: &str| {
            format!(
                r#"{{"id":"{id}","bucket":"{bucket}","run_role_arn":"r","coordinator_job_definition":"j","task_log_group":"g"}}"#
            )
        };
        let params =
            vec![(format!("{path}a"), value("a", "x")), (format!("{path}b"), value("b", "y"))];
        let p = parse_parameters(path, &params, "b").unwrap();
        assert_eq!(p.ids(), ["a", "b"]);
        assert_eq!(p.default_project().id, "b");
        let misnamed = vec![(format!("{path}a"), value("b", "y"))];
        assert!(parse_parameters(path, &misnamed, "b").is_err());
        assert!(parse_parameters(path, &[], "b").is_err());
    }

    /// The golden context the deploy's synth test compares its keys with
    /// (`crates/biffix-backend/tests/fixtures/project_context.json`).
    #[test]
    fn the_golden_project_context_parses() {
        let golden = include_str!("../../tests/fixtures/project_context.json");
        let c = ProjectContext::from_json(golden, "golden").unwrap();
        assert_eq!(c.id, "imported");
        assert_eq!(c.bucket, "acme-imported");
        assert!(c.run_role_arn.starts_with("arn:aws:iam::"));
        assert!(c.coordinator_job_definition.contains(":job-definition/"));
        assert!(!c.task_log_group.is_empty());
        assert!(c.uploads && c.viewer_cors);
        assert_eq!(c.data_sources.len(), 1);
        assert!(c.data_sources[0].viewer_cors);
        // Every key it holds is one the backend reads: nothing the deploy writes is ignored.
        let keys: Vec<String> =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(golden)
                .unwrap()
                .keys()
                .cloned()
                .collect();
        assert_eq!(
            keys,
            [
                "bucket",
                "coordinator_job_definition",
                "data_sources",
                "id",
                "run_role_arn",
                "task_log_group",
                "uploads",
                "viewer_cors"
            ]
        );
    }

    struct Flaky {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ProjectsLoader for Flaky {
        async fn load(&self) -> Result<Projects, ProjectsError> {
            // First load: project a; second: fails; third: a and b.
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(Projects::single(ctx("a", "bucket-a"))),
                1 => {
                    Err(ProjectsError::Read { path: "/p".into(), reason: "ssm unavailable".into() })
                }
                _ => Ok(two()),
            }
        }
    }

    #[tokio::test]
    async fn a_refresh_that_fails_keeps_the_last_good_projects() {
        let loader = Arc::new(Flaky { calls: AtomicUsize::new(0) });
        let handle = ProjectsHandle::load(loader.clone(), Duration::ZERO).await.unwrap();
        // Due at once (ttl 0): the refresh fails, the loaded set serves.
        assert_eq!(handle.current().await.ids(), ["a"]);
        // After a failure the next refresh waits RETRY_AFTER_FAILURE (capped at the ttl, 0 here).
        assert_eq!(handle.current().await.ids(), ["a", "b"]);
        assert_eq!(loader.calls.load(Ordering::SeqCst), 3);
        // A fixed set is never read again.
        let fixed = ProjectsHandle::fixed(two());
        assert_eq!(fixed.current().await.ids(), ["a", "b"]);
    }

    /// Loads project a first, then a and b; counts its reads.
    struct Growing {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ProjectsLoader for Growing {
        async fn load(&self) -> Result<Projects, ProjectsError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    Ok(Projects::new(vec![ctx("a", "bucket-a"), ctx("z", "bucket-z")], "a")
                        .unwrap())
                }
                _ => Ok(Projects::new(
                    vec![ctx("a", "bucket-a"), ctx("b", "bucket-b"), ctx("z", "bucket-z")],
                    "a",
                )
                .unwrap()),
            }
        }
    }

    #[tokio::test]
    async fn a_request_naming_an_unknown_project_reads_the_projects_again_once() {
        let loader = Arc::new(Growing { calls: AtomicUsize::new(0) });
        let handle = ProjectsHandle::load(loader.clone(), REFRESH_AFTER).await.unwrap();
        // Not due, but b is unknown: read again, now.
        assert!(handle.current_for(Some("b")).await.get("b").is_some());
        assert_eq!(loader.calls.load(Ordering::SeqCst), 2);
        // A known project, or none named: no read.
        handle.current_for(Some("a")).await;
        handle.current_for(None).await;
        assert_eq!(loader.calls.load(Ordering::SeqCst), 2);
        // A project still unknown right after a forced read: no second one within the gap.
        assert!(handle.current_for(Some("c")).await.get("c").is_none());
        assert_eq!(loader.calls.load(Ordering::SeqCst), 2);
    }

    /// Loads project a alone first, then a and b; counts its reads.
    struct SecondAdded {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ProjectsLoader for SecondAdded {
        async fn load(&self) -> Result<Projects, ProjectsError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(Projects::single(ctx("a", "bucket-a"))),
                _ => Ok(two()),
            }
        }
    }

    #[tokio::test]
    async fn a_request_naming_a_project_added_beside_the_only_one_reads_the_projects_again() {
        let loader = Arc::new(SecondAdded { calls: AtomicUsize::new(0) });
        let handle = ProjectsHandle::load(loader.clone(), REFRESH_AFTER).await.unwrap();
        // One project routes every label to it, but b is not it: read again, now, and route b to
        // its own project rather than a's bucket.
        let projects = handle.current_for(Some("b")).await;
        assert_eq!(projects.for_version("b").unwrap().bucket, "bucket-b");
        assert_eq!(loader.calls.load(Ordering::SeqCst), 2);
        // Another unknown label right after: no second read within the gap.
        handle.current_for(Some("c")).await;
        assert_eq!(loader.calls.load(Ordering::SeqCst), 2);
    }

    struct Hanging;

    #[async_trait]
    impl ProjectsLoader for Hanging {
        async fn load(&self) -> Result<Projects, ProjectsError> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_refresh_that_hangs_gives_up_and_keeps_the_projects() {
        let handle = ProjectsHandle {
            loaded: RwLock::new(Loaded {
                projects: Arc::new(two()),
                refresh_at: Some(Instant::now()),
                forced_at: None,
            }),
            loader: Some(Arc::new(Hanging)),
            ttl: REFRESH_AFTER,
            refreshing: tokio::sync::Mutex::new(()),
        };
        // The paused clock advances past READ_TIMEOUT once the read is the only thing waiting.
        assert_eq!(handle.current().await.ids(), ["a", "b"]);
        let refresh_at = handle.loaded.read().unwrap().refresh_at.unwrap();
        assert!(refresh_at > Instant::now(), "the next refresh waits");
    }

    #[tokio::test]
    async fn a_refresh_is_skipped_once_another_has_read_the_projects() {
        let loader = Arc::new(Growing { calls: AtomicUsize::new(0) });
        let handle = ProjectsHandle::load(loader.clone(), REFRESH_AFTER).await.unwrap();
        // Due when the request looked, read by another before it took the lock: not read twice.
        let guard = handle.refreshing.lock().await;
        assert_eq!(handle.refresh(guard, false).await.ids(), ["a", "z"]);
        assert_eq!(loader.calls.load(Ordering::SeqCst), 1);
    }
}
