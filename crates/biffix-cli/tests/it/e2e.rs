//! End-to-end verification against the REAL control-plane router + a fake OAuth token endpoint,
//! no AWS.
//!
//! The API side is served by `biffix_backend::api::router` over the backend's own in-memory
//! doubles (`biffix_backend::testing`, under its `test-support` feature) rather than a hand-written
//! JSON fake, so a wrong endpoint, query param, or response shape is caught by the actual handlers
//! — the class of drift a fake silently hides — and the doubles behave as the backend's tests pin
//! them (the event sink dedups on `dedup_key` and folds into the store, the cloud records each
//! launch). Only the OAuth token exchange is faked here, because that is an external IdP (Cognito),
//! not part of the control plane. The wire types the CLI (de)serializes are the shared `biffix-api`
//! contract the handlers produce, so a mismatch there is already a compile error; this test adds
//! the operation-level coverage on top.
//!
//! It covers the flows the CLI exists to make scriptable: bootstrap discovery, silent token
//! refresh through the one choke point, param validation + submit, and follow-to-terminal exit
//! codes. The interactive browser/paste steps are covered by unit tests (`auth::browser`,
//! `auth::paste`); here we drive everything an authenticated, non-interactive run touches.
//!
//! It is one test on purpose: the CLI reads its config directory, API URL and profile from the
//! process environment only, so scenarios split into parallel tests of this binary would race on
//! it, and the single-refresh assertion spans every command.

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::post;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use biffix_api::{OAuthClientConfig, RegisterRequest};
use biffix_backend::auth::oauth_client::OAuthClient;
use biffix_backend::aws::batch::CoordinatorLaunch;
use biffix_backend::runs::output_prefix::effective_output_prefix;
use biffix_backend::store::RunDetailView;
use biffix_backend::testing::{self, FakeCloud, MemEventSink, MemStore, TestState};
use biffix_backend::{api, AppState, DeployConfig};
use biffix_core::event::{EventData, RunSubmitted};

use biffix_cli::auth::store::{CredentialStore, Credentials, FileCredentialStore};
use biffix_cli::commands::{health, runs, workflows, GlobalArgs, Session};
use biffix_cli::error::CliError;
use biffix_cli::render::OutputFormat;
use biffix_cli::secret::Secret;

// ── The server: the real router over the backend doubles ─────────────────────────────────────

const BUCKET: &str = "test-bucket";

/// A finished run seeded into the store: of the `wf` workflow in project `p` (the seeded
/// versions'), run by `engine`, with `params` recorded.
fn finished_run(run_id: &str, state: &str, engine: Option<&str>, params: Value) -> RunDetailView {
    let mut detail = testing::run_detail(run_id, state);
    detail.run.terminal = true;
    detail.run.engine = engine.map(str::to_string);
    detail.run.workflow_name = Some("wf".into());
    detail.run.project = Some("p".into());
    detail.params = params;
    detail.output_prefix = effective_output_prefix(BUCKET, run_id, None);
    detail
}

/// What the test observes of (and steers in) the running server.
struct Server {
    base: String,
    store: Arc<MemStore>,
    events: Arc<MemEventSink>,
    cloud: Arc<FakeCloud>,
    probe: Arc<Probe>,
    /// Every form posted to the fake token endpoint, in order.
    token_forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

/// Request-level observation: which submits reached the server, and whether a polled run in
/// flight is found finished.
struct Probe {
    store: Arc<MemStore>,
    /// `POST /runs` requests that reached the server (a CLI-side rejection sends none).
    submits: AtomicU64,
    /// While set, a `GET /runs/{id}` of a run still in flight finds it succeeded: its coordinator
    /// finished by the time the CLI polls. (The doubles run no coordinator.)
    finish_on_poll: AtomicBool,
}

async fn observe(State(probe): State<Arc<Probe>>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if req.method() == axum::http::Method::POST && path == "/runs" {
        probe.submits.fetch_add(1, Ordering::SeqCst);
    }
    if req.method() == axum::http::Method::GET && probe.finish_on_poll.load(Ordering::SeqCst) {
        if let Some(run_id) = path.strip_prefix("/runs/").filter(|id| !id.contains('/')) {
            if let Some(row) = probe.store.runs.lock().unwrap().get_mut(run_id) {
                if !row.run.terminal {
                    row.run.state = "succeeded".into();
                    row.run.terminal = true;
                }
            }
        }
    }
    next.run(req).await
}

impl Server {
    fn submits(&self) -> u64 {
        self.probe.submits.load(Ordering::SeqCst)
    }

    fn launches(&self) -> Vec<CoordinatorLaunch> {
        self.cloud.launched.lock().unwrap().clone()
    }

    /// The `run_submitted` anchors recorded, in order.
    fn anchors(&self) -> Vec<RunSubmitted> {
        self.events
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match &e.data {
                EventData::RunSubmitted(s) => Some((**s).clone()),
                _ => None,
            })
            .collect()
    }
}

/// The composition over the doubles, serving `/auth/config` for a server at `base` (its fake
/// `/oauth2/token`), with a web client and a separate CLI client so a refresh sent with the wrong
/// one is caught.
fn app_state(
    base: &str,
    store: Arc<MemStore>,
    events: Arc<MemEventSink>,
    cloud: Arc<FakeCloud>,
) -> AppState {
    TestState::new()
        .with_store(store)
        .with_events(events)
        .with_cloud(cloud)
        // Auth disabled (the default): this test drives the OAuth *client* dance against the fake
        // token endpoint, but the server accepts the resulting bearer without verifying it (the
        // verifier is covered by the backend's own auth tests).
        .with_deploy(DeployConfig {
            coordinator_queue: "coordinator".into(),
            worker_image_repo: "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workers".into(),
            oauth_client: OAuthClient::Configured(Box::new(OAuthClientConfig {
                authorization_endpoint: format!("{base}/oauth2/authorize"),
                token_endpoint: format!("{base}/oauth2/token"),
                client_id: "web-test".into(),
                redirect_uri: format!("{base}/auth/callback"),
                logout_endpoint: format!("{base}/logout"),
                logout_uri: format!("{base}/signed-out"),
                scopes: vec!["openid".into(), "email".into()],
                cli_client_id: Some("cli-test".into()),
                cli_redirect_uris: vec!["http://localhost:8976/callback".into()],
                user_pool_id: None,
                region: None,
            })),
            ..Default::default()
        })
        .with_projects(testing::bucket_project(BUCKET))
        .build()
}

// ── Fake OAuth token endpoint (the one external-IdP piece the control plane does not own) ─────

fn jwt(payload: Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let body = URL_SAFE_NO_PAD.encode(payload.to_string());
    format!("{header}.{body}.")
}

type TokenForms = Arc<Mutex<Vec<HashMap<String, String>>>>;

async fn token(
    State(forms): State<TokenForms>,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Json<Value> {
    // Recorded, not asserted here: a panic in a handler surfaces as a transport error.
    forms.lock().unwrap().push(form);
    Json(json!({
        "access_token": jwt(json!({"sub": "user-1"})),
        "refresh_token": "refresh-new",
        "id_token": jwt(json!({"email": "bio@lab.example.org"})),
        "expires_in": 3600,
        "token_type": "Bearer",
    }))
}

/// Bind the real router (+ a fake `/oauth2/token` route added *after* the auth layer, so it is
/// unauthenticated) and serve it, with the finished runs the follow and resume scenarios read
/// seeded into the store.
async fn start_server() -> Server {
    // Bound as `common::serve` binds, so a test holding a closed port keeps it closed.
    let listener = crate::common::bind().await;
    let addr: SocketAddr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let store = Arc::new(MemStore::new());
    let reads = json!({"reads": "s3://bucket/in.fastq"});
    for detail in [
        finished_run("run-fail", "failed", Some("snakemake"), Value::Null),
        finished_run("run-cancel", "cancelled", Some("snakemake"), Value::Null),
        finished_run("run-nf-prior", "succeeded", Some("nextflow"), Value::Null),
        finished_run("run-nf-with-params", "succeeded", Some("nextflow"), reads.clone()),
        finished_run("run-sm-prior", "succeeded", Some("snakemake"), reads),
    ] {
        store.insert(detail);
    }
    // The sink folds what the submit records into the store, as the Postgres sink does.
    let events = Arc::new(MemEventSink::folding_into(store.clone()));
    let cloud = Arc::new(FakeCloud::new());
    let probe = Arc::new(Probe {
        store: store.clone(),
        submits: AtomicU64::new(0),
        finish_on_poll: AtomicBool::new(false),
    });
    let token_forms = TokenForms::default();
    let app = api::router(app_state(&base, store.clone(), events.clone(), cloud.clone()))
        .route("/oauth2/token", post(token).with_state(token_forms.clone()))
        .layer(axum::middleware::from_fn_with_state(probe.clone(), observe));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { base, store, events, cloud, probe, token_forms }
}

/// Seed one registered version via the REAL `POST /workflows` handler (id = `{git_sha}:{hash}`),
/// with a `form_schema` requiring the `reads` input so param validation is exercised.
async fn seed_workflow(base: &str) {
    seed_version(
        base,
        "abc",
        "snakemake",
        json!([{"name": "reads", "widget": "s3_file", "required": true,
                "is_output": false, "is_secret": false, "is_path_input": true}]),
    )
    .await;
    // A Nextflow version declaring an nf-core style required `outdir` (id `nf1:hash`).
    seed_version(
        base,
        "nf1",
        "nextflow",
        json!([{"name": "outdir", "widget": "s3_dir", "required": true,
                "is_output": true, "is_secret": false, "is_path_input": false}]),
    )
    .await;
    // A version whose required `fasta` comes from a reference bundle (id `ref1:hash`).
    seed_form(
        base,
        "ref1",
        "snakemake",
        json!({"fields": [{"name": "fasta", "widget": "s3_file", "required": true,
                           "is_output": false, "is_secret": false, "is_path_input": true}],
               "warnings": [],
               "references": {"fill": {"fasta": "fasta"}, "required": ["fasta"],
                              "label": "Reference genome"}}),
    )
    .await;
}

async fn seed_version(base: &str, git_sha: &str, engine: &str, fields: Value) {
    seed_form(base, git_sha, engine, json!({"fields": fields, "warnings": []})).await;
}

async fn seed_form(base: &str, git_sha: &str, engine: &str, form_schema: Value) {
    let req = RegisterRequest {
        name: "wf".into(),
        project: "p".into(),
        source: json!({"kind": "git", "repo": "r", "git_ref": "main"}),
        git_sha: git_sha.into(),
        source_content_hash: "hash".into(),
        image: Some(format!("sha256:{}", "1".repeat(64))),
        engine: Some(engine.into()),
        engine_version: if engine == "nextflow" { "25.10.4" } else { "9.27.0" }.into(),
        entrypoint: "Snakefile".into(),
        target: None,
        form_schema,
        queue_classes: None,
        about_markdown: None,
        registration_inputs: biffix_api::RegistrationInputs {
            source_mode: "git".into(),
            cli_version: "0.0.0".into(),
            ..Default::default()
        },
        images: Default::default(),
        nextflow_plugins: Vec::new(),
    };
    let resp = reqwest::Client::new()
        .post(format!("{base}/workflows"))
        .bearer_auth("seed")
        .json(&req)
        .send()
        .await
        .expect("seed POST /workflows");
    assert!(
        resp.status().is_success(),
        "seed register failed: {} {}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );
}

/// Create experiment `name` in project `p` through the real `POST /experiments`; its id.
async fn create_experiment(base: &str, name: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/experiments"))
        .bearer_auth("seed")
        .json(&json!({"project": "p", "name": name}))
        .send()
        .await
        .expect("POST /experiments");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let v: Value = resp.json().await.unwrap();
    v["id"].as_str().unwrap().to_string()
}

/// Archive experiment `id` through the real `PUT /experiments/{id}`.
async fn archive_experiment(base: &str, id: &str) {
    let resp = reqwest::Client::new()
        .put(format!("{base}/experiments/{id}"))
        .bearer_auth("seed")
        .json(&json!({"archived": true}))
        .send()
        .await
        .expect("PUT /experiments/{id}");
    assert!(resp.status().is_success(), "{}", resp.status());
}

/// `GET {base}{path}` through the real router, as the seeding client; its JSON body.
async fn api_get<T: serde::de::DeserializeOwned>(base: &str, path: &str) -> T {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth("seed")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn args() -> GlobalArgs {
    GlobalArgs { output: OutputFormat::Json, allow_insecure: true, ..Default::default() }
}

/// A submit of `version` with `inline` params and every other option at its default (no follow).
fn submit_opts(version: &str, inline: &[&str]) -> runs::SubmitOpts {
    runs::SubmitOpts {
        version_id: version.into(),
        params_file: None,
        inline: inline.iter().map(std::string::ToString::to_string).collect(),
        target: None,
        output_prefix: None,
        retries: None,
        keep_going: None,
        resume_from: None,
        run_name: None,
        follow: false,
        reference: None,
        retention: None,
        images: vec![],
        experiment: runs::ExperimentChoice::Unset,
    }
}

async fn submit(opts: runs::SubmitOpts) -> Result<(), CliError> {
    let mut session = Session::resolve(&args()).unwrap();
    runs::submit(&mut session, opts).await
}

/// The `BIFFIX_*` environment this test sets (and clears), restored when dropped — also when an
/// assertion panics mid-test.
struct EnvGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvGuard {
    fn set(vars: &[(&'static str, Option<&std::ffi::OsStr>)]) -> Self {
        let saved = vars
            .iter()
            .map(|(name, value)| {
                let before = std::env::var_os(name);
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
                (*name, before)
            })
            .collect();
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, before) in &self.0 {
            match before {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// Build a throwaway git repo (with an `origin` remote and one committed file) so
/// `workflows::register` can resolve real git provenance and a content hash.
fn init_workflow_repo(root: &std::path::Path) {
    let git = |args: &[&str]| crate::common::git_ok(root, args);
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    git(&["remote", "add", "origin", "https://example.com/wf.git"]);
    std::fs::write(root.join("Snakefile"), b"rule all:\n    input: []\n").unwrap();
    git(&["add", "Snakefile"]);
    git(&["commit", "-q", "-m", "init"]);
}

#[tokio::test]
async fn e2e_refresh_submit_and_follow() {
    let server = start_server().await;
    let base = server.base.clone();
    seed_workflow(&base).await;

    // Hermetic config + credentials dir; set the API base via env (precedence: env > profile).
    // A developer's own BIFFIX_PROFILE would pick another profile than the seeded "default".
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(&[
        ("BIFFIX_CONFIG_DIR", Some(dir.path().as_os_str())),
        ("BIFFIX_API_URL", Some(base.as_ref())),
        ("BIFFIX_ALLOW_INSECURE", Some("1".as_ref())),
        ("BIFFIX_PROFILE", None),
    ]);

    // Seed an EXPIRED credential with a refresh token: the choke point must refresh it silently.
    let store = FileCredentialStore::new(dir.path().join("credentials.json"));
    store
        .save(
            "default",
            &Credentials {
                access_token: Secret::new("stale".into()),
                refresh_token: Some(Secret::new("refresh-old".into())),
                expires_at_unix: 0, // long expired
                subject: "user-1".into(),
                email: Some("bio@lab.example.org".into()),
                // Issued for this deployment: the choke point only sends it there.
                api_base_url: base.clone(),
            },
        )
        .unwrap();

    // workflows list — exercises discovery + silent refresh + a bearer'd GET against the real
    // handler.
    {
        let mut session = Session::resolve(&args()).unwrap();
        workflows::list(&mut session, None).await.expect("workflows list");
    }

    // submit with valid params, following to SUCCEEDED → Ok. Hits the real get_workflow + submit
    // handlers; the run's coordinator "finishes" by the time the CLI polls it.
    {
        server.probe.finish_on_poll.store(true, Ordering::SeqCst);
        let out = submit(runs::SubmitOpts {
            // The submit-time target and retry defaults, through the real submit handler.
            target: Some("smoke".into()),
            retries: Some(5),
            keep_going: Some(false),
            follow: true,
            ..submit_opts("abc:hash:0", &["reads=s3://bucket/in.fastq"])
        })
        .await;
        server.probe.finish_on_poll.store(false, Ordering::SeqCst);
        assert!(out.is_ok(), "expected success, got {out:?}");

        // What the CLI sent reached the coordinator launch and the run's anchor.
        let launches = server.launches();
        assert_eq!(launches.len(), 1, "one coordinator launch");
        let launch = &launches[0];
        assert_eq!(launch.target.as_deref(), Some("smoke"));
        assert_eq!(launch.retries, Some(5));
        assert_eq!(launch.keep_going, Some(false));
        assert_eq!(launch.params["reads"], "s3://bucket/in.fastq");
        assert!(!launch.resume);
        let anchors = server.anchors();
        assert_eq!(anchors.len(), 1, "one run_submitted anchor");
        assert_eq!(anchors[0].workflow_version_id, "abc:hash:0");
        assert_eq!(anchors[0].params["reads"], "s3://bucket/in.fastq");
        assert_eq!(anchors[0].target.as_deref(), Some("smoke"));
        // The followed run is the one submitted, and it is the one the store shows finished.
        let runs = server.store.runs.lock().unwrap();
        let followed = runs.get(launch.run_id.as_str()).expect("the submitted run");
        assert_eq!(followed.run.state, "succeeded");
    }

    // submit missing the required param → local validation fails BEFORE any submit call. The
    // server would reject it too (with "validation failed: …"), so the CLI's own wording and the
    // absent request are what tell the two apart.
    {
        let (submits, launches) = (server.submits(), server.launches().len());
        let out = submit(submit_opts("abc:hash:0", &[])).await;
        assert!(
            matches!(&out, Err(CliError::InvalidInput(m))
                if m == "parameter validation failed: reads: required"),
            "got {out:?}"
        );
        assert_eq!(server.submits(), submits, "no POST /runs was sent");
        assert_eq!(server.launches().len(), launches);
    }

    // A Nextflow version's required `outdir` may be left unset: the server defaults it into the
    // run's output prefix, so the CLI's pre-validation must not reject what the server accepts.
    {
        let launches = server.launches().len();
        let out = submit(submit_opts("nf1:hash:0", &[])).await;
        assert!(out.is_ok(), "expected the server to default outdir, got {out:?}");
        assert_eq!(server.launches().len(), launches + 1);
    }

    // submit --resume-from reaches the real handler, which owns the resume rules: this version is
    // Snakemake but the prior run is Nextflow, so the server rejects it (400) and the CLI surfaces
    // the server's reason.
    {
        let (submits, launches) = (server.submits(), server.launches().len());
        let err = submit(runs::SubmitOpts {
            resume_from: Some("run-nf-prior".into()),
            ..submit_opts("abc:hash:0", &["reads=s3://bucket/in.fastq"])
        })
        .await
        .expect_err("a Snakemake version cannot resume a run of another engine");
        assert!(
            matches!(&err, CliError::InvalidInput(m)
                if m.contains("run run-nf-prior is a Nextflow run, not a Snakemake run")),
            "got {err:?}"
        );
        assert_eq!(err.exit_code(), biffix_cli::error::exit::INVALID_INPUT);
        assert_eq!(server.submits(), submits + 1, "the server decided");
        assert_eq!(server.launches().len(), launches);
    }

    // ...and a Snakemake version resumes a finished run of its own workflow and engine: the
    // resumed run launches in the prior run's output prefix, as a resume.
    {
        let out = submit(runs::SubmitOpts {
            resume_from: Some("run-sm-prior".into()),
            ..submit_opts("abc:hash:0", &["reads=s3://bucket/in.fastq"])
        })
        .await;
        assert!(out.is_ok(), "resume of a Snakemake run: {out:?}");
        let launches = server.launches();
        let resumed = launches.last().unwrap();
        assert!(resumed.resume);
        assert_eq!(resumed.output_prefix, effective_output_prefix(BUCKET, "run-sm-prior", None));
        let anchor = server.anchors().pop().unwrap();
        assert_eq!(anchor.resumed_from.as_deref(), Some("run-sm-prior"));
    }

    // --reference: refused locally for a version without a reference dropdown (no request sent);
    // for one with it, the params the bundle fills are the server's to resolve, so an unset
    // required `fasta` passes the local check and the server's verdict on the bundle comes back.
    for (version, local, message) in [
        ("abc:hash:0", true, "--reference: this workflow version takes no reference bundle"),
        ("ref1:hash:0", false, "unknown reference bundle grch38"),
    ] {
        let submits = server.submits();
        let err = submit(runs::SubmitOpts {
            reference: Some("grch38".into()),
            ..submit_opts(version, &["reads=s3://bucket/in.fastq"])
        })
        .await
        .expect_err("no such bundle");
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.contains(message)),
            "{version}: got {err:?}"
        );
        assert_eq!(
            server.submits(),
            submits + u64::from(!local),
            "{version}: refused locally = {local}"
        );
    }

    // --resume-from inherits the prior run's params: with no --param at all, the required `reads`
    // comes from the prior run, so local validation passes and the request reaches the server
    // (which then rejects resuming a run of another engine) instead of failing locally on a
    // missing `reads`.
    {
        let submits = server.submits();
        let err = submit(runs::SubmitOpts {
            resume_from: Some("run-nf-with-params".into()),
            ..submit_opts("abc:hash:0", &[])
        })
        .await
        .expect_err("a Snakemake version cannot resume a run of another engine");
        assert!(
            matches!(&err, CliError::InvalidInput(m)
                if m.contains("is a Nextflow run, not a Snakemake run")),
            "inherited params should pass local validation; got {err:?}"
        );
        assert_eq!(server.submits(), submits + 1, "the request reached the server");
    }

    // Experiments: --experiment launches into one; a blank one is refused before any request; a
    // resume inherits the prior run's experiment while it is open (else launches outside it, as
    // it does with --no-experiment); `runs experiment` moves a run in and out.
    {
        let live = create_experiment(&base, "Batch A").await;
        let (submits, anchors) = (server.submits(), server.anchors().len());
        let err = submit(runs::SubmitOpts {
            experiment: runs::ExperimentChoice::Into("  ".into()),
            ..submit_opts("abc:hash:0", &["reads=s3://bucket/in.fastq"])
        })
        .await
        .expect_err("a blank --experiment");
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.starts_with("--experiment: an experiment id must not be blank")),
            "got {err:?}"
        );
        assert_eq!(server.submits(), submits, "refused locally");

        let out = submit(runs::SubmitOpts {
            experiment: runs::ExperimentChoice::Into(live.clone()),
            ..submit_opts("abc:hash:0", &["reads=s3://bucket/in.fastq"])
        })
        .await;
        assert!(out.is_ok(), "{out:?}");
        let anchor = server.anchors().pop().unwrap();
        assert_eq!(anchor.experiment_id.as_deref(), Some(live.as_str()));

        // The prior run is in `live`: a resume inherits it, unless --no-experiment.
        server.store.runs.lock().unwrap().get_mut("run-sm-prior").unwrap().run.experiment_id =
            Some(live.clone());
        let resume = |experiment| runs::SubmitOpts {
            resume_from: Some("run-sm-prior".into()),
            experiment,
            ..submit_opts("abc:hash:0", &["reads=s3://bucket/in.fastq"])
        };
        // A resume holds the prior run's prefix while it runs (the server refuses another into
        // it), so each one here ends, as its coordinator would, before the next.
        let end_resumes = || {
            for d in server.store.runs.lock().unwrap().values_mut() {
                if d.resumed_from.is_some() {
                    d.run.state = "succeeded".into();
                    d.run.terminal = true;
                }
            }
        };
        end_resumes();
        submit(resume(runs::ExperimentChoice::Unset)).await.unwrap();
        let anchor = server.anchors().pop().unwrap();
        assert_eq!(anchor.resumed_from.as_deref(), Some("run-sm-prior"));
        end_resumes();
        assert_eq!(anchor.experiment_id.as_deref(), Some(live.as_str()));
        submit(resume(runs::ExperimentChoice::Outside)).await.unwrap();
        assert_eq!(server.anchors().pop().unwrap().experiment_id, None);
        // Archived since: the resume still launches, outside any experiment.
        archive_experiment(&base, &live).await;
        end_resumes();
        submit(resume(runs::ExperimentChoice::Unset)).await.unwrap();
        assert_eq!(server.anchors().pop().unwrap().experiment_id, None);
        assert_eq!(server.anchors().len(), anchors + 4);

        // `runs experiment`: in, then out (each recorded as the run's `experiment_set`). On a run
        // no later step follows: the folding double re-folds a seeded run from its log, which has
        // no anchor for it.
        let other = create_experiment(&base, "Batch B").await;
        let mut session = Session::resolve(&args()).unwrap();
        let in_store = |run: &str| server.store.runs.lock().unwrap()[run].run.experiment_id.clone();
        runs::set_experiment(&mut session, "run-nf-prior", Some(&other)).await.expect("put in");
        assert_eq!(in_store("run-nf-prior"), Some(other.clone()));
        runs::set_experiment(&mut session, "run-nf-prior", None).await.expect("taken out");
        assert_eq!(in_store("run-nf-prior"), None);
        let err = runs::set_experiment(&mut session, "run-nf-prior", Some(" "))
            .await
            .expect_err("a blank id");
        assert!(matches!(err, CliError::InvalidInput(_)), "{err:?}");
        // The server's refusal comes back: an archived experiment takes no runs.
        let err = runs::set_experiment(&mut session, "run-nf-prior", Some(&live))
            .await
            .expect_err("archived");
        assert!(matches!(&err, CliError::InvalidInput(m) if m.contains("archived")), "{err:?}");

        // `runs rename`: a name, then --clear (sent as `run_name: null`, which the server needs:
        // a body without it is refused). A blank name is refused before any request, so the name
        // is kept.
        let name_in_store = |run: &str| server.store.runs.lock().unwrap()[run].run.run_name.clone();
        runs::rename(&mut session, "run-nf-prior", Some("  Liver batch  ")).await.expect("renamed");
        assert_eq!(name_in_store("run-nf-prior").as_deref(), Some("Liver batch"));
        let err = runs::rename(&mut session, "run-nf-prior", Some("   "))
            .await
            .expect_err("a blank name");
        assert!(matches!(err, CliError::InvalidInput(_)), "{err:?}");
        assert_eq!(name_in_store("run-nf-prior").as_deref(), Some("Liver batch"));
        runs::rename(&mut session, "run-nf-prior", None).await.expect("cleared");
        assert_eq!(name_in_store("run-nf-prior"), None);
    }

    // follow a run the store reports as failed → RunFailed (exit code 8 for CI gating).
    {
        let mut session = Session::resolve(&args()).unwrap();
        let out = runs::follow(&mut session, "run-fail").await;
        assert!(matches!(out, Err(CliError::RunFailed(_))), "expected RunFailed, got {out:?}");
        assert_eq!(out.unwrap_err().exit_code(), 8);
    }

    // cancel through the real handler: its 202 carries an empty body, which the client must accept
    // (a finished run makes it a no-op: nothing is terminated).
    {
        let mut session = Session::resolve(&args()).unwrap();
        runs::cancel(&mut session, "run-cancel")
            .await
            .expect("cancel of a finished run is an accepted no-op");
        assert!(server.cloud.terminated.lock().unwrap().is_empty());
    }

    // follow a cancelled run → RunCancelled (exit code 9).
    {
        let mut session = Session::resolve(&args()).unwrap();
        let out = runs::follow(&mut session, "run-cancel").await;
        assert_eq!(out.unwrap_err().exit_code(), 9);
    }

    // workflows register: drives the CLI's register path end to end against the REAL POST
    // /workflows handler — engine detection, form-schema derivation, git provenance, the content
    // hash and version-id computation, and annotate_register — none of which `seed_workflow`'s
    // direct POST exercises.
    {
        let repo = tempfile::tempdir().unwrap();
        init_workflow_repo(repo.path());
        let opts = |path: &std::path::Path, allow_dirty: bool| workflows::RegisterOpts {
            path: path.to_string_lossy().into_owned(),
            image: Some(format!("sha256:{}", "2".repeat(64))),
            name: Some("wf-e2e".into()),
            project: None,
            queue_classes: vec![],
            allow_dirty,
            engine_version: Some("9.27.0".into()),
            entrypoint: None,
            source_mode: biffix_cli::cli::SourceMode::Git,
            // Its runtime is ready at once here (the registry double builds every variant), so
            // this exercises the wait's poll of GET /workflows/{id}.
            wait: Some(std::time::Duration::from_secs(60)),
        };
        let mut session = Session::resolve(&args()).unwrap();
        let out = workflows::register(&mut session, opts(repo.path(), true)).await;
        assert!(out.is_ok(), "register against the real router failed: {out:?}");
        // A deploy of one project: without --project, the version is registered into it.
        let registered: Vec<Value> = api_get(&base, "/workflows").await;
        let wf = registered.iter().find(|v| v["name"] == "wf-e2e").expect("wf-e2e registered");
        assert_eq!(wf["project"], "demo", "{wf}");
        // The same source with the same inputs again is refused with the server's reason, which
        // names the version already registered, and no queue-class/image hint.
        let out = workflows::register(&mut session, opts(repo.path(), false)).await;
        assert!(
            matches!(out, Err(CliError::InvalidInput(ref m))
                if m.starts_with("identical to build 0 (") && !m.contains("queue-class")),
            "an identical re-register: {out:?}"
        );

        // In git mode the coordinator runs the clone's root at the commit: a dirty tree (even
        // with --allow-dirty) or a subdirectory would register an id that no run executes.
        std::fs::write(repo.path().join("Snakefile"), b"rule all:\n    input: ['x']\n").unwrap();
        let out = workflows::register(&mut session, opts(repo.path(), true)).await;
        assert!(
            matches!(out, Err(CliError::InvalidInput(ref m)) if m.contains("--source-mode s3")),
            "a dirty git-mode register: {out:?}"
        );
        let sub = repo.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Snakefile"), b"rule all:\n    input: []\n").unwrap();
        let out = workflows::register(&mut session, opts(&sub, true)).await;
        assert!(
            matches!(out, Err(CliError::InvalidInput(ref m)) if m.contains("subdirectory `sub`")),
            "a git-mode register of a subdirectory: {out:?}"
        );

        // A repository whose only Snakefile is `workflow/Snakefile` records that, not the
        // `Snakefile` the coordinator would never find; an entrypoint not in the commit is
        // refused before anything is registered.
        let layout = tempfile::tempdir().unwrap();
        init_workflow_repo(layout.path());
        std::fs::create_dir(layout.path().join("workflow")).unwrap();
        crate::common::git_ok(layout.path(), &["mv", "Snakefile", "workflow/Snakefile"]);
        crate::common::git_ok(layout.path(), &["commit", "-q", "-m", "standard layout"]);
        let layout_opts = |entrypoint: Option<&str>| workflows::RegisterOpts {
            name: Some("wf-layout".into()),
            entrypoint: entrypoint.map(str::to_string),
            wait: None,
            ..opts(layout.path(), false)
        };
        let out = workflows::register(&mut session, layout_opts(Some("Snakefile"))).await;
        assert!(
            matches!(out, Err(CliError::InvalidInput(ref m))
                if m.contains("`Snakefile` (from --entrypoint)")),
            "a register naming a missing entrypoint: {out:?}"
        );
        let layouts = || async {
            api_get::<Vec<Value>>(&base, "/workflows")
                .await
                .into_iter()
                .filter(|v| v["name"] == "wf-layout")
                .collect::<Vec<Value>>()
        };
        assert!(layouts().await.is_empty(), "the refused register registered nothing");
        let out = workflows::register(&mut session, layout_opts(None)).await;
        assert!(out.is_ok(), "register of workflow/Snakefile: {out:?}");
        let versions = layouts().await;
        assert_eq!(versions.len(), 1, "{versions:?}");
        let id = versions[0]["id"].as_str().unwrap();
        let detail: Value = api_get(&base, &format!("/workflows/{id}")).await;
        assert_eq!(detail["entrypoint"], "workflow/Snakefile", "{detail}");
    }

    // workflows reregister: the newest `wf` on another engine release registers again as its next
    // build; the version `register` just froze has nothing new (up to date); a dry run of every
    // workflow registers nothing.
    {
        let before = api_get::<Vec<Value>>(&base, "/workflows").await.len();
        let opts = |target: Option<&str>, all: bool, dry_run: bool| workflows::ReregisterOpts {
            target: target.map(str::to_string),
            all,
            project: None,
            engine_version: None,
            dry_run,
            wait: Some(std::time::Duration::from_secs(60)),
        };
        let mut session = Session::resolve(&args()).unwrap();
        let out = workflows::reregister(&mut session, opts(None, true, true)).await;
        assert!(out.is_ok(), "reregister --all --dry-run: {out:?}");
        let out = workflows::reregister(&mut session, opts(Some("wf-e2e"), false, false)).await;
        assert!(out.is_ok(), "reregister of an up-to-date version: {out:?}");
        let mut onto_another_release = opts(Some("abc:hash:0"), false, false);
        onto_another_release.engine_version = Some("9.27.1".into());
        let out = workflows::reregister(&mut session, onto_another_release).await;
        assert!(out.is_ok(), "reregister of a version: {out:?}");
        let after: Vec<Value> = api_get(&base, "/workflows").await;
        assert_eq!(after.len(), before + 1);
        assert!(after.iter().any(|v| v["id"] == "abc:hash:1"), "{after:?}");
    }

    // workflows release / unrelease: through the real `PUT /workflows/{id}/release`, recorded with
    // who and when; a blank id is refused before any request, an unknown one is the server's 404.
    {
        let mut session = Session::resolve(&args()).unwrap();
        let base = base.as_str();
        let release = |released: bool| async move {
            api_get::<Value>(base, "/workflows/abc:hash:0")
                .await
                .pointer("/release/released")
                .cloned()
                == Some(json!(released))
        };
        let out = workflows::release(&mut session, "abc:hash:0", true).await;
        assert!(out.is_ok(), "release: {out:?}");
        assert!(release(true).await);
        let listed: Vec<Value> = api_get(base, "/workflows?released=true").await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0]["id"], "abc:hash:0");
        assert_eq!(listed[0]["release"]["by"], "local");
        let out = workflows::list(&mut session, Some(true)).await;
        assert!(out.is_ok(), "list --released: {out:?}");
        let out = workflows::show(&mut session, "abc:hash:0").await;
        assert!(out.is_ok(), "show a released version: {out:?}");
        let out = workflows::release(&mut session, "abc:hash:0", false).await;
        assert!(out.is_ok(), "unrelease: {out:?}");
        assert!(release(false).await);
        let err = workflows::release(&mut session, "  ", true).await.expect_err("a blank id");
        assert!(matches!(err, CliError::InvalidInput(_)), "{err:?}");
        let err = workflows::release(&mut session, "nope:hash:0", true)
            .await
            .expect_err("an unknown version");
        assert!(matches!(err, CliError::NotFound(_)), "{err:?}");
    }

    // health: both unauthenticated probes hit the real /healthz + /readyz (200), and the
    // authenticated /status is folded in — a signed-in, reachable deploy exits Ok.
    {
        let mut session = Session::resolve(&args()).unwrap();
        let out = health::health(&mut session).await;
        assert!(out.is_ok(), "health against a live deploy should be Ok: {out:?}");
    }

    // The stale access token was refreshed and persisted (expiry advanced from 0), and the fake
    // token endpoint was actually hit.
    let refreshed = store.load("default").unwrap().unwrap();
    assert!(refreshed.expires_at_unix > 0, "credential was refreshed and saved");
    // Exactly one refresh: every later command reused the persisted token rather than
    // refreshing again, and the rotated refresh token replaced the old one.
    let forms = server.token_forms.lock().unwrap().clone();
    assert_eq!(forms.len(), 1, "the token endpoint is hit once, by the first command");
    // A public-client refresh: the CLI client id (not the web client's), the stored refresh
    // token, and never a client secret.
    let form = &forms[0];
    assert_eq!(form.get("grant_type").map(String::as_str), Some("refresh_token"));
    assert_eq!(form.get("client_id").map(String::as_str), Some("cli-test"));
    assert_eq!(form.get("refresh_token").map(String::as_str), Some("refresh-old"));
    assert!(!form.contains_key("client_secret"), "{form:?}");
    assert_eq!(refreshed.refresh_token.as_ref().map(|t| t.expose().as_str()), Some("refresh-new"));
    assert_eq!(refreshed.api_base_url, base);
}
