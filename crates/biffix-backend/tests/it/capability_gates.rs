//! The capability gates on every protected route, driven with real (statically signed) principals.
//!
//! One table ([`protected_routes`]) lists each protected operation with the weakest role its
//! capability admits. It is checked against the OpenAPI document and the router's own source, so a
//! route added to either without a row here fails the suite. Over that table: no token is a 401,
//! a token whose groups map to no role is a 403, and each route refuses every role below its
//! capability's minimum and admits every role at or above it. Plus the reference-bundle edit rules
//! that need two different callers.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use utoipa::OpenApi;

use biffix_backend::api;
use biffix_backend::auth::rbac::Role;
use biffix_backend::testing::{self, call_as, FakeCloud, MemStore, TestState};
use biffix_core::ids::CoordinatorJobId;

const SECRET: &[u8] = b"capability-gates-secret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_gates";
const AUD: &str = "biffix-web";

/// Each role's group, weakest first.
const ROLES: [(Role, &str); 4] = [
    (Role::Viewer, "viewers"),
    (Role::Operator, "operators"),
    (Role::Developer, "developers"),
    (Role::Admin, "admins"),
];

/// A group no role is mapped from.
const UNMAPPED: &str = "some-other-group";

/// The routes served without the Cognito middleware (`/events` checks its own capability token).
const OPEN: [(&str, &str); 4] =
    [("GET", "/auth/config"), ("GET", "/healthz"), ("GET", "/readyz"), ("POST", "/events")];

/// A router over `store` and `cloud`, with run `r1` running under a coordinator so an admitted
/// cancel has a job to terminate.
fn app_over(store: MemStore, cloud: Arc<FakeCloud>) -> axum::Router {
    let mut r1 = testing::run_detail("r1", "running");
    r1.coordinator_job_id = Some(CoordinatorJobId::new("coord-1"));
    store.insert(r1);
    let groups = ROLES.map(|(role, group)| (group, role));
    let auth = testing::static_auth(SECRET, ISS, vec![AUD.into()], &groups);
    api::router(
        TestState::new()
            .with_store(Arc::new(store))
            .with_cloud(cloud)
            .with_auth(auth)
            // Uploads on, so an upload route's only possible 403 is its capability gate.
            .with_projects(testing::one_project(biffix_backend::config::projects::ProjectContext {
                uploads: true,
                ..testing::project("demo", "proj")
            }))
            .build(),
    )
}

fn app() -> axum::Router {
    app_over(MemStore::new(), Arc::new(FakeCloud::new()))
}

/// A token for `sub` in `group`.
fn token(sub: &str, group: &str) -> String {
    testing::user_token(SECRET, ISS, AUD, sub, &[group])
}

fn bundle(display_name: &str) -> Value {
    json!({
        "id": "grch38",
        "kind": "assembly",
        "display_name": display_name,
        "uris": {"fasta": "s3://proj/refs/hg38.fa"},
    })
}

/// One protected operation: its method, its path as the OpenAPI document spells it, a concrete
/// request (the smallest body and query its extractors accept, so the request reaches the
/// handler's gate), and the weakest role its capability admits.
struct Route {
    method: Method,
    path: &'static str,
    uri: String,
    body: Option<Value>,
    weakest: Role,
}

fn route(
    method: Method,
    path: &'static str,
    uri: &str,
    body: Option<Value>,
    weakest: Role,
) -> Route {
    Route { method, path, uri: uri.to_string(), body, weakest }
}

/// Every protected operation the API serves. `the_route_table_is_every_protected_operation` keeps
/// this in step with the router and the OpenAPI document.
fn protected_routes() -> Vec<Route> {
    use Method as M;
    use Role::{Admin, Developer, Operator, Viewer};
    let key = "uploads/sample.fastq.gz";
    let upload = "/data/uploads/up-1";
    let file = json!({"source": "project", "path": "a.txt"});
    vec![
        // ViewRuns: every role.
        route(M::GET, "/runs", "/runs", None, Viewer),
        route(M::POST, "/runs/query", "/runs/query", Some(json!({})), Viewer),
        route(M::GET, "/runs/{run_id}", "/runs/r1", None, Viewer),
        route(M::GET, "/runs/{run_id}", "/runs/r1?tasks=false", None, Viewer),
        route(
            M::POST,
            "/runs/{run_id}/tasks/query",
            "/runs/r1/tasks/query",
            Some(json!({})),
            Viewer,
        ),
        route(
            M::POST,
            "/runs/{run_id}/task-logs/query",
            "/runs/r1/task-logs/query",
            Some(json!({"task": "t"})),
            Viewer,
        ),
        route(
            M::POST,
            "/runs/{run_id}/attempts/query",
            "/runs/r1/attempts/query",
            Some(json!({"task": "t"})),
            Viewer,
        ),
        route(M::GET, "/runs/{run_id}/failure-summary", "/runs/r1/failure-summary", None, Viewer),
        route(M::GET, "/runs/{run_id}/samples", "/runs/r1/samples", None, Viewer),
        route(M::GET, "/runs/{run_id}/coordinator-logs", "/runs/r1/coordinator-logs", None, Viewer),
        route(
            M::POST,
            "/runs/{run_id}/outputs/query",
            "/runs/r1/outputs/query",
            Some(json!({})),
            Viewer,
        ),
        route(M::GET, "/runs/{run_id}/reports", "/runs/r1/reports", None, Viewer),
        route(M::GET, "/runs/{run_id}/provenance", "/runs/r1/provenance", None, Viewer),
        route(M::GET, "/runs/{run_id}/qc/general-stats", "/runs/r1/qc/general-stats", None, Viewer),
        route(M::GET, "/runs/{run_id}/qc/fastqc-status", "/runs/r1/qc/fastqc-status", None, Viewer),
        route(M::GET, "/runs/{run_id}/outputs/{output_id}", "/runs/r1/outputs/abc", None, Viewer),
        route(
            M::GET,
            "/runs/{run_id}/outputs/{output_id}/download",
            "/runs/r1/outputs/abc/download",
            None,
            Viewer,
        ),
        route(
            M::GET,
            "/runs/{run_id}/outputs/{output_id}/view-url",
            "/runs/r1/outputs/abc/view-url",
            None,
            Viewer,
        ),
        route(
            M::GET,
            "/runs/{run_id}/outputs/{output_id}/preview",
            "/runs/r1/outputs/abc/preview",
            None,
            Viewer,
        ),
        route(
            M::GET,
            "/runs/{run_id}/outputs/{output_id}/fastqc",
            "/runs/r1/outputs/abc/fastqc",
            None,
            Viewer,
        ),
        route(
            M::GET,
            "/runs/{run_id}/outputs/{output_id}/alignment-view",
            "/runs/r1/outputs/abc/alignment-view",
            None,
            Viewer,
        ),
        route(M::GET, "/usage", "/usage", None, Viewer),
        route(M::GET, "/data-sources", "/data-sources", None, Viewer),
        route(M::GET, "/projects", "/projects", None, Viewer),
        route(M::POST, "/data/list", "/data/list", Some(json!({"source": "project"})), Viewer),
        route(
            M::POST,
            "/data/find",
            "/data/find",
            Some(json!({"source": "project", "q": "a"})),
            Viewer,
        ),
        route(M::POST, "/data/head", "/data/head", Some(json!({"uris": []})), Viewer),
        route(M::POST, "/data/preview", "/data/preview", Some(file.clone()), Viewer),
        route(M::POST, "/data/view-url", "/data/view-url", Some(file.clone()), Viewer),
        route(M::POST, "/data/download-url", "/data/download-url", Some(file), Viewer),
        route(M::POST, "/samples/detect", "/samples/detect", Some(json!({"files": []})), Viewer),
        route(M::GET, "/references", "/references", None, Viewer),
        route(M::GET, "/workflows", "/workflows", None, Viewer),
        route(M::GET, "/workflows/{id}", "/workflows/v1", None, Viewer),
        route(M::GET, "/workflows/{id}/estimate", "/workflows/v1/estimate?samples=2", None, Viewer),
        route(M::GET, "/engines", "/engines", None, Viewer),
        route(M::GET, "/status", "/status", None, Viewer),
        route(M::GET, "/status/history", "/status/history", None, Viewer),
        route(M::GET, "/settings", "/settings", None, Viewer),
        route(M::GET, "/me/prefs", "/me/prefs", None, Viewer),
        route(M::GET, "/me/notifications", "/me/notifications", None, Viewer),
        route(
            M::POST,
            "/me/notifications/read",
            "/me/notifications/read",
            Some(json!({"all": true})),
            Viewer,
        ),
        route(
            M::POST,
            "/me/attention/dismiss",
            "/me/attention/dismiss",
            Some(json!({"run_ids": ["r1"]})),
            Viewer,
        ),
        route(M::GET, "/experiments", "/experiments", None, Viewer),
        route(M::GET, "/experiments/{id}", "/experiments/exp-1", None, Viewer),
        route(
            M::GET,
            "/experiments/{id}/samples.csv",
            "/experiments/exp-1/samples.csv",
            None,
            Viewer,
        ),
        route(M::PUT, "/me/prefs", "/me/prefs", Some(json!({"view": "expert"})), Viewer),
        // LaunchWorkflow: operator and up.
        route(
            M::POST,
            "/runs",
            "/runs",
            Some(json!({"workflow_version_id": "v1", "params": {}})),
            Operator,
        ),
        route(M::POST, "/runs/{run_id}/cancel", "/runs/r1/cancel", None, Operator),
        route(
            M::POST,
            "/runs/{run_id}/experiment",
            "/runs/r1/experiment",
            Some(json!({"experiment_id": null})),
            Operator,
        ),
        route(
            M::POST,
            "/runs/{run_id}/name",
            "/runs/r1/name",
            Some(json!({"run_name": "renamed"})),
            Operator,
        ),
        route(
            M::POST,
            "/experiments",
            "/experiments",
            Some(json!({"project": "demo", "name": "Batch 1"})),
            Operator,
        ),
        route(
            M::PUT,
            "/experiments/{id}",
            "/experiments/exp-1",
            Some(json!({"name": "Batch 2"})),
            Operator,
        ),
        route(
            M::POST,
            "/data/uploads",
            "/data/uploads",
            Some(json!({"key": key, "size": 10})),
            Operator,
        ),
        route(
            M::POST,
            "/data/uploads/{upload_id}/parts",
            &format!("{upload}/parts"),
            Some(json!({"key": key, "part_numbers": [1]})),
            Operator,
        ),
        route(
            M::POST,
            "/data/uploads/{upload_id}/status",
            &format!("{upload}/status"),
            Some(json!({"key": key})),
            Operator,
        ),
        route(
            M::POST,
            "/data/uploads/{upload_id}/complete",
            &format!("{upload}/complete"),
            Some(json!({"key": key, "parts": [{"part_number": 1, "etag": "e"}]})),
            Operator,
        ),
        route(
            M::POST,
            "/data/uploads/{upload_id}/abort",
            &format!("{upload}/abort"),
            Some(json!({"key": key})),
            Operator,
        ),
        // RegisterWorkflow: developer and up.
        route(
            M::POST,
            "/workflows",
            "/workflows",
            Some(json!({
                "name": "w", "project": "demo", "source": {}, "git_sha": "a",
                "source_content_hash": "h", "engine_version": "9.0.0",
                "entrypoint": "Snakefile", "form_schema": {},
                "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
            })),
            Developer,
        ),
        route(
            M::PUT,
            "/workflows/listing",
            "/workflows/listing",
            Some(json!({"project": "demo", "name": "w", "hidden": true})),
            Developer,
        ),
        route(
            M::PUT,
            "/workflows/{id}/release",
            "/workflows/v1/release",
            Some(json!({"released": true})),
            Developer,
        ),
        route(
            M::POST,
            "/workflows/source-upload",
            "/workflows/source-upload",
            Some(json!({"version_id": "v1", "project": "demo"})),
            Developer,
        ),
        route(
            M::POST,
            "/workflows/{id}/reregister",
            "/workflows/v1/reregister",
            Some(json!({})),
            Developer,
        ),
        route(
            M::POST,
            "/workflows/{id}/runtime/rebuild",
            "/workflows/v1/runtime/rebuild",
            None,
            Developer,
        ),
        route(
            M::POST,
            "/workflows/{id}/images/{name}/rebuild",
            "/workflows/v1/images/x/rebuild",
            None,
            Developer,
        ),
        route(M::PUT, "/settings", "/settings", Some(json!({"title": "Lab"})), Developer),
        route(M::GET, "/settings/alerts", "/settings/alerts", None, Developer),
        route(
            M::POST,
            "/settings/alerts",
            "/settings/alerts",
            Some(json!({"email": "ops@example.org"})),
            Developer,
        ),
        route(
            M::POST,
            "/settings/alerts/remove",
            "/settings/alerts/remove",
            Some(json!({"email": "ops@example.org"})),
            Developer,
        ),
        // ManageReferenceBundles: admin only.
        route(M::POST, "/references", "/references", Some(bundle("Human GRCh38")), Admin),
        route(
            M::PUT,
            "/references/{id}",
            "/references/grch38",
            Some(bundle("Human GRCh38")),
            Admin,
        ),
        route(M::DELETE, "/references/{id}", "/references/grch38", None, Admin),
    ]
}

/// Every (method, path) the OpenAPI document declares, paths as it spells them (`{param}`).
fn openapi_operations() -> BTreeSet<(String, String)> {
    let mut ops = BTreeSet::new();
    for (path, item) in api::ApiDoc::openapi().paths.paths {
        let unsupported = [&item.options, &item.head, &item.trace, &item.query];
        assert!(
            unsupported.iter().all(|op| op.is_none()) && item.additional_operations.is_empty(),
            "{path}: an operation other than GET, POST, PUT, DELETE or PATCH"
        );
        let declared = [
            ("GET", &item.get),
            ("POST", &item.post),
            ("PUT", &item.put),
            ("DELETE", &item.delete),
            ("PATCH", &item.patch),
        ];
        for (method, op) in declared {
            if op.is_some() {
                ops.insert((method.to_string(), path.clone()));
            }
        }
    }
    ops
}

/// The source of every route table `api::router` merges: each family module's `routes()` and
/// `open_routes()` (`src/api/<family>.rs`), each up to its closing brace.
fn route_tables() -> Vec<String> {
    let api = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let mut tables = Vec::new();
    for entry in std::fs::read_dir(&api).expect("src/api") {
        let src = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for start in ["fn routes() ->", "fn open_routes() ->"] {
            if let Some(at) = src.find(start) {
                let body = &src[at..];
                tables.push(body[..body.find("\n}").expect("end of a route table")].to_string());
            }
        }
    }
    assert!(tables.len() > 1, "no family route tables found under {api:?}");
    tables
}

/// Every (method, path) `api::router` mounts, read from the families' route tables (axum cannot
/// enumerate a router's routes): each `.route("path", get(..).post(..))` in [`route_tables`] (axum
/// spells a parameter `{param}`, as OpenAPI does).
fn router_operations() -> BTreeSet<(String, String)> {
    let mut ops = BTreeSet::new();
    let tables = route_tables();
    for chunk in tables.iter().flat_map(|t| t.split(".route(").skip(1)) {
        let path = chunk.split('"').nth(1).expect("a route's path literal").to_string();
        let before_len = ops.len();
        for method in ["get", "post", "put", "delete", "patch"] {
            let call = format!("{method}(");
            // `get(` itself, not the tail of a longer name (`budget(`).
            let mounted = chunk.match_indices(&call).any(|(at, _)| {
                !chunk[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            if mounted {
                ops.insert((method.to_uppercase(), path.clone()));
            }
        }
        assert!(ops.len() > before_len, "no method read for route {path}");
    }
    ops
}

#[test]
fn the_route_table_is_every_protected_operation() {
    let open: BTreeSet<(String, String)> =
        OPEN.iter().map(|(m, p)| (m.to_string(), p.to_string())).collect();
    let router = router_operations();
    let documented = openapi_operations();
    // Each route the router mounts is documented, and each documented one is mounted.
    let undocumented: Vec<_> = router.difference(&documented).collect();
    let unmounted: Vec<_> = documented.difference(&router).collect();
    assert!(
        undocumented.is_empty() && unmounted.is_empty(),
        "api::router mounts {undocumented:?} that ApiDoc lacks; ApiDoc lists unmounted {unmounted:?}"
    );
    assert!(open.is_subset(&router), "{open:?} vs {router:?}");

    let table: BTreeSet<(String, String)> =
        protected_routes().iter().map(|r| (r.method.to_string(), r.path.to_string())).collect();
    let protected: BTreeSet<_> = router.difference(&open).cloned().collect();
    let missing: Vec<_> = protected.difference(&table).collect();
    let extra: Vec<_> = table.difference(&protected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "protected_routes() is missing {missing:?} and lists unrouted {extra:?}"
    );
}

#[tokio::test]
async fn every_protected_route_refuses_a_caller_without_a_token_or_a_mapped_group() {
    let unmapped = token("u", UNMAPPED);
    for Route { method, uri, body, .. } in protected_routes() {
        let (status, v) = call_as(app(), method.clone(), &uri, body.clone(), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {v}");
        let (status, v) = call_as(app(), method.clone(), &uri, body, Some(&unmapped)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {v}");
    }
}

#[tokio::test]
async fn each_route_admits_exactly_the_roles_its_capability_allows() {
    for Route { method, uri, body, weakest, .. } in protected_routes() {
        for (role, group) in ROLES {
            let (status, v) =
                call_as(app(), method.clone(), &uri, body.clone(), Some(&token("u", group))).await;
            assert_ne!(status, StatusCode::UNAUTHORIZED, "{method} {uri} as {group}");
            if role < weakest {
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri} as {group}: {v}");
            } else {
                assert_ne!(status, StatusCode::FORBIDDEN, "{method} {uri} as {group}: {v}");
            }
        }
    }
}

#[tokio::test]
async fn only_an_admin_sets_the_support_contact() {
    let app = app();
    let put = |body: Value, group: &'static str| {
        let app = app.clone();
        async move { call_as(app, Method::PUT, "/settings", Some(body), Some(&token("u", group))).await }
    };
    let support = json!({"support": {"name": "Core", "email": "core@lab.example.org"}});
    // A Developer edits the title, but not the support contact (nor clears it).
    let (status, v) = put(support.clone(), "developers").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, v) = put(json!({"title": "Lab", "support": null}), "developers").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, v) = put(json!({"title": "Lab"}), "developers").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"title": "Lab"}));
    // An Admin does.
    let (status, v) = put(support, "admins").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["support"], json!({"name": "Core", "email": "core@lab.example.org"}));
    assert_eq!(v["title"], "Lab");
}

#[tokio::test]
async fn a_viewer_cannot_cancel_a_run_and_an_operator_can() {
    let cloud = Arc::new(FakeCloud::new());
    let app = app_over(MemStore::new(), cloud.clone());
    let (status, v) =
        call_as(app.clone(), Method::POST, "/runs/r1/cancel", None, Some(&token("u", "viewers")))
            .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(cloud.terminated.lock().unwrap().is_empty());

    let (status, v) =
        call_as(app, Method::POST, "/runs/r1/cancel", None, Some(&token("u", "operators"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let terminated = cloud.terminated.lock().unwrap();
    assert_eq!(terminated.len(), 1, "{terminated:?}");
    assert_eq!(terminated[0].0, "coord-1");
}

#[tokio::test]
async fn a_refusal_names_the_missing_capability() {
    let (status, v) = call_as(
        app(),
        Method::PUT,
        "/settings",
        Some(json!({"title": "Lab"})),
        Some(&token("u", "viewers")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(v.as_str().unwrap().contains("RegisterWorkflow"), "{v}");
}

#[tokio::test]
async fn an_edit_keeps_who_created_a_bundle_and_when() {
    let app = app();
    let (status, created) = call_as(
        app.clone(),
        Method::POST,
        "/references",
        Some(bundle("Human GRCh38")),
        Some(&token("creator", "admins")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let created_at = created["bundle"]["created_at_micros"].as_i64().unwrap();

    // A different admin edits it, a moment later.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let (status, saved) = call_as(
        app.clone(),
        Method::PUT,
        "/references/grch38",
        Some(bundle("Human GRCh38 (GATK)")),
        Some(&token("editor", "admins")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let saved = &saved["bundle"];
    assert_eq!(saved["display_name"], "Human GRCh38 (GATK)");
    assert_eq!(saved["created_by"], "creator");
    assert_eq!(saved["created_at_micros"].as_i64(), Some(created_at));
    assert!(saved["updated_at_micros"].as_i64().unwrap() > created_at);

    // What the list serves agrees with what the edit returned.
    let (_, listed) =
        call_as(app.clone(), Method::GET, "/references", None, Some(&token("viewer", "viewers")))
            .await;
    assert_eq!(&listed[0], saved);

    // An edit of a bundle that does not exist is a 404, not a create.
    let mut other = bundle("Mouse");
    other["id"] = json!("grcm39");
    let (status, _) = call_as(
        app,
        Method::PUT,
        "/references/grcm39",
        Some(other),
        Some(&token("editor", "admins")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
