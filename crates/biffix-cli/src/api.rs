//! The typed control-plane API client.
//!
//! Bears the **access** token (never an ID token), resolved through the command's
//! [`TokenSource`] so a long-running command refreshes it as it goes (and replays a request once
//! after a `401`). Maps `401`/`403`/`404`/`400`/`5xx` to the error taxonomy, and retries with
//! jittered exponential backoff — but a request that is not safe to repeat (`POST /runs`,
//! `POST /workflows`, reregister) is only retried when it provably never reached the server.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Serialize;

use biffix_api::{
    EngineSupport, ExperimentList, Project, RegisterRequest, ReregisterPlan, ReregisterRequest,
    RunDetail, RunExperimentSetRequest, RunFilter, RunPage, RunRenameRequest, RunSubmitRequest,
    RunSummary, SourceUploadRequest, SourceUploadTarget, StatusReport, SubmittedRun,
    WorkflowRelease, WorkflowReleaseUpdateRequest, WorkflowVersionDetail, WorkflowVersionSummary,
};

use crate::auth::TokenSource;
use crate::config::DiscoveredConfig;
use crate::error::{CliError, CliResult};
use crate::secret::Secret;

/// What a transport failure talking to the control plane says it could not reach.
const API: &str = "the biffix API";

/// How many times a retryable failure is retried before giving up.
const MAX_RETRIES: u32 = 4;
const BASE_BACKOFF: Duration = Duration::from_millis(200);
/// The per-attempt timeout of a JSON API call.
const JSON_TIMEOUT: Duration = Duration::from_secs(30);
/// A presigned upload's per-attempt timeout is this floor plus the time the body takes at
/// [`UPLOAD_FLOOR_BYTES_PER_SEC`]: a large tarball on a slow link is never cut off at a fixed
/// total, while a stalled small upload still fails in minutes.
const UPLOAD_BASE_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_FLOOR_BYTES_PER_SEC: u64 = 128 * 1024;

/// Where an [`ApiClient`] gets its bearer token.
enum Bearer {
    /// A fixed token. Test support: no command builds one (every command's client bears a
    /// [`TokenSource`]), but the integration tests drive the client against stubs with it.
    Static(Secret<String>),
    /// The command's refreshing token source.
    Source(Arc<TokenSource>),
}

/// Whether a request may be sent again after a failure that might have reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Safe to repeat (a GET, a write-once presigned PUT, a presign request, a cancel): retry every
    /// transient failure — transport faults, `429`, `502`, `503`, `504`.
    Always,
    /// Not safe to repeat (it creates something): retry only a failure that cannot have reached
    /// the server — a connect error or a `429` throttle. A timeout, a failure after the request
    /// was sent, or a `5xx` may have been applied, so it surfaces as `Unavailable` naming `check`,
    /// the command that shows whether it was.
    OnlyUnsent { check: &'static str },
}

pub struct ApiClient {
    http: reqwest::Client,
    base: String,
    bearer: Bearer,
    verbose: bool,
}

impl ApiClient {
    /// A client bearing a fixed `access_token`, never refreshed. Test support (see `Bearer::Static`);
    /// a command's client comes from `Session::authed_client`.
    pub fn new(
        http: reqwest::Client,
        base_url: &str,
        access_token: Secret<String>,
        verbose: bool,
    ) -> Self {
        Self::with_bearer(http, base_url, Bearer::Static(access_token), verbose)
    }

    /// A client that resolves (and refreshes) its bearer through `source` for every request.
    pub fn with_token_source(
        http: reqwest::Client,
        base_url: &str,
        source: TokenSource,
        verbose: bool,
    ) -> Self {
        Self::with_bearer(http, base_url, Bearer::Source(Arc::new(source)), verbose)
    }

    fn with_bearer(http: reqwest::Client, base_url: &str, bearer: Bearer, verbose: bool) -> Self {
        Self { http, base: base_url.trim_end_matches('/').to_string(), bearer, verbose }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// Fetch the public OAuth config (`GET /auth/config`) — no auth required (bootstrap).
    /// A failure carries the server's own reason (e.g. why the OAuth client is unconfigured).
    /// With `verbose`, each attempt is logged as every other call's is.
    pub async fn discover(
        http: &reqwest::Client,
        base_url: &str,
        verbose: bool,
    ) -> CliResult<DiscoveredConfig> {
        let url = format!("{}/auth/config", base_url.trim_end_matches('/'));
        let label = format!("GET {url}");
        let resp = send_with_retry(verbose, &label, API, Retry::Always, || {
            http.get(&url).timeout(Duration::from_secs(20))
        })
        .await?;
        let status = resp.status();
        let text = read_body(resp, &url).await?;
        if !status.is_success() {
            return Err(match map_status(status, &text, "") {
                CliError::NotFound(_) => CliError::Unavailable(format!(
                    "{url} was not found ({status}); is {base_url} a biffix API base URL?"
                )),
                other => other.map_message(|m| format!("GET /auth/config failed: {m}")),
            });
        }
        let discovered: DiscoveredConfig = serde_json::from_str(&text).map_err(|e| {
            CliError::Unclassified(format!("/auth/config returned unparseable JSON: {e}"))
        })?;
        // Every login and refresh discovers through here, so no path uses an unchecked endpoint.
        discovered.validate(base_url.trim_end_matches('/'))?;
        Ok(discovered)
    }

    /// GET a JSON resource.
    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> CliResult<T> {
        self.request_json::<(), T>(reqwest::Method::GET, path, None, Retry::Always).await
    }

    /// POST a JSON body and deserialize the JSON response.
    async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        retry: Retry,
    ) -> CliResult<T> {
        self.request_json::<B, T>(reqwest::Method::POST, path, Some(body), retry).await
    }

    /// PUT a JSON body and deserialize the JSON response.
    async fn put_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        retry: Retry,
    ) -> CliResult<T> {
        self.request_json::<B, T>(reqwest::Method::PUT, path, Some(body), retry).await
    }

    async fn bearer(&self) -> CliResult<Secret<String>> {
        match &self.bearer {
            Bearer::Static(token) => Ok(token.clone()),
            Bearer::Source(source) => source.bearer().await,
        }
    }

    async fn request_json<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
        retry: Retry,
    ) -> CliResult<T> {
        let url = self.url(path);
        let label = format!("{method} {url}");
        let mut token = self.bearer().await?;
        let mut replayed = false;
        let resp = loop {
            let resp = send_with_retry(self.verbose, &label, API, retry, || {
                let mut req = self
                    .http
                    .request(method.clone(), &url)
                    .bearer_auth(token.expose())
                    .timeout(JSON_TIMEOUT);
                if let Some(b) = body {
                    req = req.json(b);
                }
                req
            })
            .await?;
            // A 401 is refused before any handler runs, so even a non-idempotent request is safe
            // to replay once with a refreshed token.
            match &self.bearer {
                Bearer::Source(source)
                    if resp.status() == StatusCode::UNAUTHORIZED && !replayed =>
                {
                    token = source.after_unauthorized(&token).await?;
                    replayed = true;
                }
                _ => break resp,
            }
        };
        let status = resp.status();
        // A request not safe to repeat that the server answered with a success or a 5xx may have
        // been applied, so a reply that cannot be read says so, naming where to check.
        let applied = |err: CliError| match retry {
            Retry::OnlyUnsent { check } if status.is_success() || may_have_applied(status) => {
                may_have_applied_err(&label, &err.to_string(), check)
            }
            _ => err,
        };
        let text = read_body(resp, &url).await.map_err(applied)?;
        if status.is_success() {
            // An empty body (e.g. a 202 from cancel) is JSON `null`, which deserializes as `()`;
            // otherwise parse T. (serde_json rejects an empty string outright.)
            let text = if text.trim().is_empty() { "null" } else { text.as_str() };
            return serde_json::from_str(text).map_err(|e| {
                applied(CliError::Unclassified(format!("unparseable response from {url}: {e}")))
            });
        }
        // The server's (plain-text) error body tells the user exactly what it rejected —
        // "workflow version … already exists", "unknown run …" — not just the status class.
        Err(applied(map_status(status, &text, &self.verbose_hint())))
    }

    fn verbose_hint(&self) -> String {
        if self.verbose {
            format!(" ({})", self.base)
        } else {
            String::new()
        }
    }

    // ── Typed endpoints ──────────────────────────────────────────────────────────
    pub async fn list_workflows(&self) -> CliResult<Vec<WorkflowVersionSummary>> {
        self.list_workflows_released(None).await
    }

    /// The registered versions, only the released (`Some(true)`) or only the developer
    /// (`Some(false)`) ones (`GET /workflows?released=`), or every one (`None`).
    pub(crate) async fn list_workflows_released(
        &self,
        released: Option<bool>,
    ) -> CliResult<Vec<WorkflowVersionSummary>> {
        match released {
            Some(r) => self.get_json(&format!("/workflows?released={r}")).await,
            None => self.get_json("/workflows").await,
        }
    }

    /// The deploy's projects (`GET /projects`).
    pub(crate) async fn list_projects(&self) -> CliResult<Vec<Project>> {
        self.get_json("/projects").await
    }

    pub async fn list_engines(&self) -> CliResult<Vec<EngineSupport>> {
        self.get_json("/engines").await
    }

    pub(crate) async fn get_workflow(&self, id: &str) -> CliResult<WorkflowVersionDetail> {
        self.get_json(&format!("/workflows/{}", urlencoding::encode(id))).await
    }

    /// Release a version (or, `false`, un-release it) with `PUT /workflows/{id}/release`, and
    /// return the version's release after the change. Safe to repeat: the same change again
    /// records nothing.
    pub(crate) async fn set_workflow_released(
        &self,
        id: &str,
        released: bool,
    ) -> CliResult<WorkflowRelease> {
        self.put_json(
            &format!("/workflows/{}/release", urlencoding::encode(id)),
            &WorkflowReleaseUpdateRequest { released },
            Retry::Always,
        )
        .await
    }

    pub async fn register_workflow(
        &self,
        req: &RegisterRequest,
    ) -> CliResult<WorkflowVersionSummary> {
        self.post_json("/workflows", req, Retry::OnlyUnsent { check: "biffix workflows list" })
            .await
    }

    /// Register a version again from what it recorded. The request is always sent with
    /// `dry_run: false` whatever `req` says, so the response is the registered version; use
    /// [`Self::reregister_plan`] for a preview.
    pub(crate) async fn reregister_workflow(
        &self,
        id: &str,
        req: &ReregisterRequest,
    ) -> CliResult<WorkflowVersionSummary> {
        self.reregister(id, req, false, Retry::OnlyUnsent { check: "biffix workflows list" }).await
    }

    /// What re-registering a version would do. The request is always sent with `dry_run: true`
    /// whatever `req` says, so the response is a plan and nothing is registered; being read-only,
    /// it is retried like a GET.
    pub(crate) async fn reregister_plan(
        &self,
        id: &str,
        req: &ReregisterRequest,
    ) -> CliResult<ReregisterPlan> {
        self.reregister(id, req, true, Retry::Always).await
    }

    /// POST `req` to re-register version `id`, with its `dry_run` set to `dry_run`.
    async fn reregister<T: DeserializeOwned>(
        &self,
        id: &str,
        req: &ReregisterRequest,
        dry_run: bool,
        retry: Retry,
    ) -> CliResult<T> {
        let req = ReregisterRequest { dry_run, ..req.clone() };
        self.post_json(&format!("/workflows/{}/reregister", urlencoding::encode(id)), &req, retry)
            .await
    }

    /// Ask the backend to presign a PUT for staging a workflow-source tarball (S3-staged source).
    /// Presigning is side-effect free, so it is retried like a GET.
    pub(crate) async fn source_upload(
        &self,
        req: &SourceUploadRequest,
    ) -> CliResult<SourceUploadTarget> {
        self.post_json("/workflows/source-upload", req, Retry::Always).await
    }

    /// PUT raw bytes to a presigned S3 URL (no bearer auth — the URL is self-authorizing), with the
    /// headers its signature covers. Retried like a GET: the key is content-addressed (and usually
    /// write-once), so a repeat writes the same bytes. Used to upload a staged source or
    /// build-context tarball. `false` when the PUT is write-once and the key already holds an
    /// object (412): it is staged already, and is not replaced.
    pub async fn put_presigned(
        &self,
        upload: &SourceUploadTarget,
        body: impl Into<Bytes>,
    ) -> CliResult<bool> {
        let url = &upload.url;
        // The query is the URL's signature (and any session token): what is logged and reported
        // names the object only.
        let shown = without_query(url);
        // `Bytes` clones are refcounted, so a retry never copies the body again.
        let body: Bytes = body.into();
        let timeout = upload_timeout(body.len());
        // A presigned URL points at the object store, not the API: a failure names its host.
        let target = match url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string))
        {
            Some(host) => format!("the upload host {host}"),
            None => "the upload URL".to_string(),
        };
        let label = format!("PUT {shown}");
        let resp = send_with_retry(self.verbose, &label, &target, Retry::Always, || {
            let mut put = self.http.put(url).body(body.clone()).timeout(timeout);
            for (name, value) in &upload.headers {
                put = put.header(name, value);
            }
            put
        })
        .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == StatusCode::PRECONDITION_FAILED
            && upload.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
        {
            return Ok(false);
        }
        let detail = read_body(resp, &shown).await?;
        Err(map_status(status, &detail, &self.verbose_hint()))
    }

    pub(crate) async fn submit_run(&self, req: &RunSubmitRequest) -> CliResult<SubmittedRun> {
        self.post_json("/runs", req, Retry::OnlyUnsent { check: "biffix runs list" }).await
    }

    pub async fn list_runs(&self, state: Option<RunFilter>) -> CliResult<Vec<RunSummary>> {
        // `/runs` returns a paginated `RunPage { runs, total, limit, last_cursor, .. }`, filtered
        // by `filter=` (not `state=`). Follow the keyset cursor (`after` = the older edge) to
        // exhaustion so a scriptable `runs list` returns EVERY matching run, not a silent first
        // page. The server may clamp the page below what was asked, so a page is "full" when it
        // holds as many rows as the `limit` the server echoes back, not as many as requested.
        const PAGE: i64 = 200;
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut path = format!("/runs?limit={PAGE}");
            if let Some(s) = state {
                path.push_str(&format!("&filter={}", s.as_str()));
            }
            if let Some(cursor) = &after {
                path.push_str(&format!("&after={}", urlencoding::encode(cursor)));
            }
            let page: RunPage = self.get_json(&path).await?;
            let count = page.runs.len() as i64;
            let served = page.limit;
            all.extend(page.runs);
            // An empty or short page (fewer rows than the server's page size), or a missing
            // cursor, is the last page.
            match page.last_cursor {
                Some(cursor) if count > 0 && count >= served => after = Some(cursor),
                _ => break,
            }
        }
        Ok(all)
    }

    /// Cancel a run (`POST /runs/{id}/cancel`): terminates its coordinator; the run then ends as
    /// cancelled. Cancelling is idempotent (a finished run is a no-op), so it is retried like a
    /// GET.
    pub(crate) async fn cancel_run(&self, run_id: &str) -> CliResult<()> {
        self.post_json(&format!("/runs/{}/cancel", urlencoding::encode(run_id)), &(), Retry::Always)
            .await
    }

    pub(crate) async fn get_run(&self, run_id: &str) -> CliResult<RunDetail> {
        self.get_json(&format!("/runs/{}", urlencoding::encode(run_id))).await
    }

    /// The live (not archived) experiments of `project` (`GET /experiments?project=`).
    pub(crate) async fn list_experiments(&self, project: &str) -> CliResult<ExperimentList> {
        self.get_json(&format!("/experiments?project={}", urlencoding::encode(project))).await
    }

    /// Put `run_id` in `experiment_id`, or (`None`) take it out of its experiment
    /// (`POST /runs/{id}/experiment`); the run's summary after. Safe to repeat: the same change
    /// again folds to the same membership.
    pub(crate) async fn set_run_experiment(
        &self,
        run_id: &str,
        experiment_id: Option<&str>,
    ) -> CliResult<RunSummary> {
        self.post_json(
            &format!("/runs/{}/experiment", urlencoding::encode(run_id)),
            &RunExperimentSetRequest { experiment_id: experiment_id.map(str::to_string) },
            Retry::Always,
        )
        .await
    }

    /// Rename `run_id`, or (`None`) clear its name (`POST /runs/{id}/name`); the run's summary
    /// after. Safe to repeat: the same name again folds to the same name.
    pub(crate) async fn rename_run(
        &self,
        run_id: &str,
        run_name: Option<&str>,
    ) -> CliResult<RunSummary> {
        self.post_json(
            &format!("/runs/{}/name", urlencoding::encode(run_id)),
            &RunRenameRequest { run_name: run_name.map(str::to_string) },
            Retry::Always,
        )
        .await
    }

    /// The authenticated service-status report (`GET /status`). Requires `ViewRuns`.
    pub(crate) async fn get_status(&self) -> CliResult<StatusReport> {
        self.get_json("/status").await
    }
}

/// A status worth retrying for a request that is safe to repeat: a throttle, or a gateway /
/// availability error that may not recur a moment later.
fn is_retryable(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

/// A final status that says a non-idempotent request may nonetheless have been applied: the
/// backend can do part of the work before a 5xx, and a gateway timeout says nothing either way.
fn may_have_applied(status: StatusCode) -> bool {
    status.is_server_error()
}

/// One attempt's outcome, as the retry predicate sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Status(StatusCode),
    /// The connection was never established: nothing was sent.
    ConnectFailed,
    /// A timeout or a failure after the request was (or may have been) sent.
    FailedInFlight,
}

/// Whether to retry after `attempt` under `retry` (the attempt budget is checked separately).
fn should_retry(retry: Retry, attempt: Attempt) -> bool {
    match (retry, attempt) {
        (Retry::Always, Attempt::Status(s)) => is_retryable(s),
        (Retry::Always, Attempt::ConnectFailed | Attempt::FailedInFlight) => true,
        (Retry::OnlyUnsent { .. }, Attempt::Status(s)) => s == StatusCode::TOO_MANY_REQUESTS,
        (Retry::OnlyUnsent { .. }, Attempt::ConnectFailed) => true,
        (Retry::OnlyUnsent { .. }, Attempt::FailedInFlight) => false,
    }
}

fn classify(e: &reqwest::Error) -> Attempt {
    // A connect timeout is both `is_connect` and `is_timeout`; either way nothing was sent.
    if e.is_connect() {
        Attempt::ConnectFailed
    } else {
        Attempt::FailedInFlight
    }
}

fn may_have_applied_err(label: &str, what: &str, check: &str) -> CliError {
    CliError::Unavailable(format!(
        "{label}: {what}; the request may have been applied — check `{check}` before retrying"
    ))
}

/// `url` without its query or fragment, for a log line or an error: a presigned URL's query is a
/// credential. A URL that does not parse is cut at its first `?` or `#`.
fn without_query(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.split(['?', '#']).next().unwrap_or("").to_string(),
    }
}

/// Read a response body; a failure to read it is a transport failure, not an empty body.
async fn read_body(resp: reqwest::Response, url: &str) -> CliResult<String> {
    resp.text().await.map_err(|e| {
        CliError::Unavailable(format!(
            "the response from {url} could not be read: {}",
            CliError::unreachable("the server", &e)
        ))
    })
}

/// A presigned upload's per-attempt timeout: [`UPLOAD_BASE_TIMEOUT`] plus the body's transfer
/// time at [`UPLOAD_FLOOR_BYTES_PER_SEC`].
fn upload_timeout(len: usize) -> Duration {
    UPLOAD_BASE_TIMEOUT + Duration::from_secs(len as u64 / UPLOAD_FLOOR_BYTES_PER_SEC)
}

/// The request-correlation header a response carries, if any (API Gateway's, or a generic one).
fn correlation_id(resp: &reqwest::Response) -> Option<String> {
    ["apigw-requestid", "x-amzn-requestid", "x-request-id"]
        .iter()
        .find_map(|h| resp.headers().get(*h))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Send a request — rebuilt each attempt by `build` (a `reqwest::RequestBuilder` is single-use) —
/// retrying what [`should_retry`] allows under `retry` with jittered backoff, up to
/// `MAX_RETRIES`. Returns the final response (of any status) or a classified transport error. This
/// is the ONE retry loop every call uses, so the retry predicate cannot drift between them; each
/// caller keeps its own status/body handling. A transport failure names `target`, what could not
/// be reached. With `verbose`, each attempt's `label` (its method
/// and URL), status and correlation id go to stderr, never a header: so `label` must carry no
/// secret either (a presigned URL is labelled without its query).
async fn send_with_retry<F>(
    verbose: bool,
    label: &str,
    target: &str,
    retry: Retry,
    build: F,
) -> CliResult<reqwest::Response>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let mut attempt = 0;
    loop {
        let outcome = build().send().await;
        if verbose {
            match &outcome {
                Ok(resp) => {
                    let id = correlation_id(resp)
                        .map(|id| format!(" [request id {id}]"))
                        .unwrap_or_default();
                    eprintln!("[biffix] {label} → {} (attempt {}){id}", resp.status(), attempt + 1);
                }
                Err(e) => eprintln!(
                    "[biffix] {label} → {} (attempt {})",
                    CliError::unreachable(target, e),
                    attempt + 1
                ),
            }
        }
        let seen = match &outcome {
            Ok(resp) => Attempt::Status(resp.status()),
            Err(e) => classify(e),
        };
        if attempt < MAX_RETRIES && should_retry(retry, seen) {
            backoff(attempt).await;
            attempt += 1;
            continue;
        }
        return match outcome {
            Ok(resp) => Ok(resp),
            Err(e) => {
                let err = CliError::unreachable(target, &e);
                match retry {
                    Retry::OnlyUnsent { check } if seen == Attempt::FailedInFlight => {
                        Err(may_have_applied_err(label, &err.to_string(), check))
                    }
                    _ => Err(err),
                }
            }
        };
    }
}

/// Map an HTTP status (and the server's plain-text error `body`, when non-empty) to the CLI error
/// taxonomy. The backend returns `(StatusCode, String)` errors — a plain-text reason — so the body
/// IS the actionable detail; surface it verbatim rather than only the status class.
fn map_status(status: StatusCode, body: &str, ctx: &str) -> CliError {
    let body = body.trim();
    let detail = |fallback: &str| {
        if body.is_empty() {
            fallback.to_string()
        } else {
            body.to_string()
        }
    };
    match status {
        StatusCode::UNAUTHORIZED => CliError::AuthRequired(format!(
            "{}; run `biffix login`",
            detail("authentication failed or expired")
        )),
        StatusCode::FORBIDDEN => {
            CliError::Forbidden(detail("forbidden: your role is too low for this operation"))
        }
        StatusCode::NOT_FOUND => CliError::NotFound(detail("not found")),
        StatusCode::BAD_REQUEST | StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY => {
            CliError::InvalidInput(detail(&format!("the server rejected the request ({status})")))
        }
        // Exactly the statuses `is_retryable` retries: one classification, one retry policy.
        s if is_retryable(s) => CliError::Unavailable(format!(
            "{}{ctx}",
            detail(&format!("service unavailable ({status})"))
        )),
        other => CliError::Unclassified(format!(
            "{}{ctx}",
            detail(&format!("unexpected response {other}"))
        )),
    }
}

/// Sleep for a jittered exponential backoff before the next retry.
async fn backoff(attempt: u32) {
    let exp = BASE_BACKOFF.saturating_mul(2u32.saturating_pow(attempt));
    let jitter_ms = rand::random_range(0..=(exp.as_millis() as u64 / 2 + 1));
    tokio::time::sleep(exp + Duration::from_millis(jitter_ms)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_covers_the_taxonomy() {
        assert!(matches!(map_status(StatusCode::UNAUTHORIZED, "", ""), CliError::AuthRequired(_)));
        assert!(matches!(map_status(StatusCode::FORBIDDEN, "", ""), CliError::Forbidden(_)));
        assert!(matches!(map_status(StatusCode::NOT_FOUND, "", ""), CliError::NotFound(_)));
        assert!(matches!(map_status(StatusCode::BAD_REQUEST, "", ""), CliError::InvalidInput(_)));
        assert!(matches!(
            map_status(StatusCode::SERVICE_UNAVAILABLE, "", ""),
            CliError::Unavailable(_)
        ));
        assert!(matches!(
            map_status(StatusCode::INTERNAL_SERVER_ERROR, "", ""),
            CliError::Unclassified(_)
        ));
    }

    #[test]
    fn map_status_surfaces_the_server_body() {
        // A plain-text 409 body reaches the message verbatim (not just "409 Conflict").
        let err = map_status(StatusCode::CONFLICT, "workflow version abc:hash already exists", "");
        match err {
            CliError::InvalidInput(m) => {
                assert!(m.contains("already exists"), "got {m:?}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        // A 404 with an empty body falls back to the generic message.
        assert!(matches!(
            map_status(StatusCode::NOT_FOUND, "", ""),
            CliError::NotFound(m) if m == "not found"
        ));
    }

    #[test]
    fn retryable_statuses_are_the_transient_ones_and_map_to_unavailable() {
        for s in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(is_retryable(s), "{s}");
            assert!(matches!(map_status(s, "", ""), CliError::Unavailable(_)), "{s}");
        }
        for s in [StatusCode::OK, StatusCode::NOT_FOUND, StatusCode::INTERNAL_SERVER_ERROR] {
            assert!(!is_retryable(s), "{s}");
        }
    }

    #[test]
    fn a_non_idempotent_request_is_retried_only_when_it_was_never_sent() {
        let unsafe_ = Retry::OnlyUnsent { check: "x" };
        // Never reached the server: safe to repeat.
        assert!(should_retry(unsafe_, Attempt::ConnectFailed));
        assert!(should_retry(unsafe_, Attempt::Status(StatusCode::TOO_MANY_REQUESTS)));
        // May have been applied: never repeated.
        assert!(!should_retry(unsafe_, Attempt::FailedInFlight));
        for s in [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::BAD_GATEWAY,
            StatusCode::GATEWAY_TIMEOUT,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(!should_retry(unsafe_, Attempt::Status(s)), "{s}");
            assert!(may_have_applied(s), "{s}");
        }
        assert!(!should_retry(unsafe_, Attempt::Status(StatusCode::OK)));
        // A safe request retries every transient failure, and nothing else.
        assert!(should_retry(Retry::Always, Attempt::FailedInFlight));
        assert!(should_retry(Retry::Always, Attempt::ConnectFailed));
        assert!(should_retry(Retry::Always, Attempt::Status(StatusCode::GATEWAY_TIMEOUT)));
        assert!(!should_retry(Retry::Always, Attempt::Status(StatusCode::BAD_REQUEST)));
    }

    #[test]
    fn a_shown_url_drops_its_query() {
        assert_eq!(
            without_query(
                "https://b.s3.amazonaws.com/sources/a:h/source.tar.gz?X-Amz-Signature=abc\
                 &X-Amz-Security-Token=t#frag"
            ),
            "https://b.s3.amazonaws.com/sources/a:h/source.tar.gz"
        );
        assert_eq!(without_query("not a url?X-Amz-Signature=abc"), "not a url");
    }

    #[test]
    fn the_upload_timeout_grows_with_the_body() {
        assert_eq!(upload_timeout(0), UPLOAD_BASE_TIMEOUT);
        // 1 GiB at the 128 KiB/s floor: 8192 s on top of the base.
        assert_eq!(upload_timeout(1 << 30), UPLOAD_BASE_TIMEOUT + Duration::from_secs(8192));
    }

    #[tokio::test]
    async fn an_id_is_one_path_segment_whatever_it_holds() {
        // The raw request target each call sent, as the server saw it.
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let record = seen.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let record = record.clone();
            async move {
                record.lock().unwrap().push(uri.to_string());
                axum::http::StatusCode::NOT_FOUND
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ApiClient::new(reqwest::Client::new(), &base, Secret::new("t".into()), false);
        // A version id's `:`, and a `/`, space or `?` a mistyped id may hold, never leave the
        // segment; unreserved bytes pass as they are.
        let _ = client.get_workflow("git123:hash-_.~").await;
        let _ = client.get_run("a b/c?d").await;
        assert_eq!(*seen.lock().unwrap(), ["/workflows/git123%3Ahash-_.~", "/runs/a%20b%2Fc%3Fd"]);
    }
}
