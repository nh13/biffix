//! [`OutputStore`]: a run's outputs, their cached S3 HEADs, and the output-prefix claim.

use async_trait::async_trait;
use biffix_core::ids::RunId;

use super::{Claim, StoreError};

/// The output roles a run's reports and QC lookups are found among ([`OutputStore::outputs_like_in`]):
/// what the run published or declared as an output, not its tasks' intermediates or logs.
pub(crate) const REPORT_ROLES: [&str; 2] = ["published", "output"];

/// One stored output of a run (`run_output`), before the API shapes it into an `RunOutput`.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputRow {
    pub id: String,
    pub s3_uri: String,
    pub path: Option<String>,
    pub task_key: Option<String>,
    pub attempt: Option<i64>,
    pub role: String,
    pub reused: bool,
    pub size: Option<i64>,
    /// The etag the engine reported when it produced the object, if it knew one.
    pub etag: Option<String>,
    /// What S3 last said about the object (`s3_object_meta`); `None` until first checked.
    pub meta: Option<ObjectMeta>,
    /// The run that registered the row: in a read over an [`OutputScope`] spanning a resume chain,
    /// the run itself for its own rows, else the nearest run it resumed that registered the URI.
    pub run_id: RunId,
}

/// A cached S3 HEAD of one object (`s3_object_meta`).
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectMeta {
    /// False when the HEAD found no object.
    pub present: bool,
    pub size: Option<i64>,
    pub etag: Option<String>,
    /// Epoch micros.
    pub last_modified: Option<i64>,
    /// Epoch micros of the HEAD.
    pub checked_at: i64,
}

/// How long a "missing" result is trusted before the object is checked again: a live run's file may
/// appear after it was first listed. A present object's facts are not re-checked on a timer.
pub const MISSING_RECHECK_MICROS: i64 = 24 * 3600 * 1_000_000;

impl ObjectMeta {
    /// Whether this cached HEAD should be redone at `now_micros`.
    pub(crate) fn is_stale(&self, now_micros: i64) -> bool {
        !self.present && now_micros - self.checked_at >= MISSING_RECHECK_MICROS
    }
}

/// Whether an output row still needs a HEAD: never checked, or a stale "missing".
pub(crate) fn needs_head(meta: Option<&ObjectMeta>, now_micros: i64) -> bool {
    meta.is_none_or(|m| m.is_stale(now_micros))
}

/// Whether `uri` names an object biffix can HEAD and serve: a key (not a "directory") in `bucket`,
/// outside the staged workflow-source prefix, whose tarballs may be private repositories.
pub(crate) fn checkable(uri: &str, bucket: &str) -> bool {
    let Some((b, key)) = biffix_core::s3_layout::split_s3_uri(uri) else {
        return false;
    };
    b == bucket
        && !key.is_empty()
        && !key.ends_with('/')
        && !key.starts_with(crate::aws::objects::SOURCE_PREFIX)
}

/// The [`OutputQuery::roles`] entry that admits every role.
pub const ALL_ROLES: &str = "*";

/// Filters and paging for a run's outputs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputQuery {
    /// Only this task's outputs.
    pub task: Option<String>,
    /// Only these roles; `None` ⇒ every role except `intermediate` (work-dir files nobody wants in
    /// a results list by default). A list holding [`ALL_ROLES`] admits every role (the role is an
    /// open enum, so no list of known roles covers them all).
    pub roles: Option<Vec<String>>,
    /// Only objects whose `s3_uri` starts with this (the caller resolves it against the run's
    /// output prefix, so a client cannot widen it past the run).
    pub uri_prefix: Option<String>,
    /// Keyset cursor: the last `s3_uri` of the previous page.
    pub after: Option<String>,
    pub limit: i64,
    /// The bucket biffix can HEAD (the project bucket). [`OutputPage::unchecked`] counts only
    /// objects there and outside [`crate::aws::objects::SOURCE_PREFIX`] — the ones the API would check —
    /// so it can reach zero. `None` ⇒ nothing is checkable.
    pub checkable_bucket: Option<String>,
}

impl OutputQuery {
    /// Whether `row` passes the filters (not the paging). The Postgres store expresses the same
    /// predicate in SQL; in-memory stores use this so both agree.
    // In-memory stores are test doubles; the Postgres store has its own SQL for this.
    #[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
    pub(crate) fn matches(&self, row: &OutputRow) -> bool {
        self.task.as_deref().is_none_or(|t| row.task_key.as_deref() == Some(t))
            && match &self.roles {
                Some(roles) => roles.iter().any(|r| r == ALL_ROLES || *r == row.role),
                None => row.role != "intermediate",
            }
            && self.uri_prefix.as_deref().is_none_or(|p| row.s3_uri.starts_with(p))
    }
}

/// One page of a run's outputs plus the count matching the filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputPage {
    pub rows: Vec<OutputRow>,
    pub total: i64,
    /// How many checkable outputs ([`OutputQuery::checkable_bucket`]) matching the filters still
    /// need a HEAD ([`needs_head`]), across all pages.
    pub unchecked: i64,
}

/// Whose output rows a read covers: a run's own ([`Self::run_id`]), then, nearest first, those of
/// the runs it resumed (`resumed_from`, walked), which count only under [`Self::shared_prefix`]
/// (the output prefix they share with the run: the files a resume reused live there). Of the rows
/// for one URI the nearest run's wins (the run's own over any it resumed), before any filter
/// applies. Resolved at read time rather than folded into the run's projection, so the projection
/// stays a function of the run's own log.
///
/// Built only through [`Self::run`] and [`Self::resumed`], so it always names its run, and a
/// shared prefix exactly when it spans more than one run.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputScope {
    /// The run itself first, then the runs it resumed, nearest first.
    runs: Vec<RunId>,
    /// `Some` exactly when `runs` has more than the run itself.
    shared_prefix: Option<String>,
}

impl OutputScope {
    /// Just `run_id`'s own rows.
    pub fn run(run_id: &RunId) -> Self {
        Self { runs: vec![run_id.clone()], shared_prefix: None }
    }

    /// `run_id`'s own rows, then those of `origins` (the runs it resumed, nearest first) under
    /// `shared_prefix` (the output prefix as recorded: a folder ending in `/`, or a legacy one
    /// without, which the coordinator appended its folders to directly). With no origins, just
    /// the run's own.
    pub fn resumed(run_id: &RunId, origins: Vec<RunId>, shared_prefix: &str) -> Self {
        if origins.is_empty() {
            return Self::run(run_id);
        }
        let mut runs = Vec::with_capacity(origins.len() + 1);
        runs.push(run_id.clone());
        runs.extend(origins);
        Self { runs, shared_prefix: Some(shared_prefix.to_string()) }
    }

    /// The run whose outputs these are.
    pub(crate) fn run_id(&self) -> &RunId {
        &self.runs[0]
    }

    /// The run itself, then the runs it resumed whose rows count, nearest first.
    pub(crate) fn runs(&self) -> &[RunId] {
        &self.runs
    }

    /// The prefix the resumed runs' rows count under; `None` for a lone run.
    pub(crate) fn shared_prefix(&self) -> Option<&str> {
        self.shared_prefix.as_deref()
    }

    /// Only the run's own rows count.
    pub(crate) fn is_lone(&self) -> bool {
        self.runs.len() == 1
    }
}

/// A run's outputs (`run_output`, its own and those it reuses from the runs it resumed), the S3
/// HEAD results cached for them, and the claim a submit holds on its output prefix.
#[async_trait]
pub trait OutputStore: Send + Sync {
    /// A page of the run's outputs, ordered by `s3_uri`, matching `query`. An unknown run yields an
    /// empty page (the caller has already loaded the run).
    async fn list_outputs(
        &self,
        run_id: &RunId,
        query: &OutputQuery,
    ) -> Result<OutputPage, StoreError> {
        self.list_outputs_in(&OutputScope::run(run_id), query).await
    }

    /// [`Self::list_outputs`] over `scope`'s rows: `total` and `unchecked` count them too.
    async fn list_outputs_in(
        &self,
        scope: &OutputScope,
        query: &OutputQuery,
    ) -> Result<OutputPage, StoreError>;

    /// `scope`'s published/output-role outputs whose `s3_uri` matches any of the SQL `LIKE`
    /// `patterns` (escape `\\`), ordered by `s3_uri`, at most `limit`: the candidates for its
    /// reports. The default filters a full listing; Postgres does it in SQL.
    async fn outputs_like_in(
        &self,
        scope: &OutputScope,
        patterns: &[String],
        limit: i64,
    ) -> Result<Vec<OutputRow>, StoreError> {
        let page = self
            .list_outputs_in(
                scope,
                &OutputQuery {
                    roles: Some(REPORT_ROLES.map(String::from).to_vec()),
                    limit: i64::MAX,
                    ..Default::default()
                },
            )
            .await?;
        Ok(page
            .rows
            .into_iter()
            .filter(|r| patterns.iter().any(|p| biffix_workflow::glob::like_matches(p, &r.s3_uri)))
            .take(usize::try_from(limit).unwrap_or(0))
            .collect())
    }

    /// One of `scope`'s outputs by its opaque id: `None` when the id is not one of them (another
    /// run's, a resumed run's row outside the shared prefix, or one a nearer run's row for its URI
    /// hides).
    async fn get_output_in(
        &self,
        scope: &OutputScope,
        id: &str,
    ) -> Result<Option<OutputRow>, StoreError>;

    /// `scope`'s output at `uri`, of any role: the nearest of its runs' rows for the URI (the
    /// run's own first), in one read. `None` when none of them registered it where it counts.
    async fn output_at_in(
        &self,
        scope: &OutputScope,
        uri: &str,
    ) -> Result<Option<OutputRow>, StoreError>;

    /// Record S3 HEAD results, one per URI, keeping the newest per URI: a result checked before
    /// the one held (an overlapping request that HEADed first and wrote last) does not replace it.
    async fn put_object_meta(&self, metas: &[(String, ObjectMeta)]) -> Result<(), StoreError>;

    /// A run not yet ended (not succeeded, failed or cancelled) whose output prefix is exactly
    /// `output_prefix`, if any. Submit refuses a resume into a prefix another run is still using:
    /// the two would share its work dir and the engine's resume cache.
    async fn active_run_in_prefix(&self, output_prefix: &str) -> Result<Option<RunId>, StoreError>;

    /// Claim `output_prefix` for one submit, so its check for an active run there
    /// ([`Self::active_run_in_prefix`]) and its recording of the new run cannot interleave with
    /// another submit's: the claim is held until the returned guard is dropped. `None` when another
    /// submit holds it now (the caller refuses rather than waits).
    async fn claim_output_prefix(&self, output_prefix: &str) -> Result<Option<Claim>, StoreError>;
}
