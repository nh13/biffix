//! HTTP tests for the smaller REST routes: liveness, readiness and the status page,
//! `/auth/config`, site settings, usage, the caller's own (`me`: prefs, notifications), samples,
//! and the task list's parameter errors. In-memory doubles, with auth disabled except where two
//! users matter (`authed_app`: prefs, notifications, attention); the capability gates are
//! `capability_gates.rs`'s. The run, task, log and output reads, including the task list's
//! filters and paging, are the `http_reads` module's.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use serde_json::json;

use biffix_backend::auth::oauth_client::OAuthClient;
use biffix_backend::store::cost::CostRow;
use biffix_backend::store::status::StatusBucketView;
use biffix_backend::testing::{self, call, get_json, post_json, MemStore, TestState};
use biffix_backend::{api, AppState, DeployConfig};

mod me;
mod notifications;
mod samples;
mod settings;
mod status;
mod usage;

fn state(store: MemStore, deploy: DeployConfig) -> AppState {
    TestState::new().with_store(Arc::new(store)).with_deploy(deploy).build()
}

fn app_with(store: MemStore) -> axum::Router {
    api::router(state(store, DeployConfig::default()))
}

/// An app over a store holding run `r1`, submitted.
fn app_with_one_run() -> axum::Router {
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "submitted"));
    app_with(store)
}

const SECRET: &[u8] = b"rest-api-test-secret";
const ISS: &str = "https://issuer.test";
const AUD: &str = "biffix-test";

fn authed_app(store: MemStore) -> axum::Router {
    let auth = testing::static_auth(
        SECRET,
        ISS,
        vec![AUD.into()],
        &[("viewers", biffix_backend::auth::rbac::Role::Viewer)],
    );
    api::router(TestState::new().with_store(Arc::new(store)).with_auth(auth).build())
}

fn token(sub: &str) -> String {
    testing::user_token(SECRET, ISS, AUD, sub, &["viewers"])
}
