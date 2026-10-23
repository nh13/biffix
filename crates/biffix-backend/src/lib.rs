//! biffix control-plane API library.
//!
//! axum + utoipa. The HTTP surface ([`api::router`], which merges each handler family's route
//! table) reads runs, tasks, outputs, reports, provenance, usage and service status, and writes
//! through run submit and cancel, workflow registration, reference bundles, site settings and
//! alert recipients, browser uploads into the project bucket, and engine event ingest
//! (`POST /events`, per-run capability token). The AWS boundary and the stores are traits, so every
//! handler is testable without AWS or Postgres.

#![forbid(unsafe_code)]
// Without `aws` (the OpenAPI exporter, the in-memory test build) the production wiring, the AWS
// provider and the Postgres stores are compiled out, which leaves the crate-private helpers only
// they call unused. Dead code is still caught: a helper nothing calls is dead with
// every feature on too, and the all-features build denies it.
#![cfg_attr(not(feature = "aws"), allow(dead_code))]
// This is an application crate: its `pub` items exist for its binaries and tests, not for outside
// users, and its docs are built with `--document-private-items`, where a link from a public item's
// docs to a crate-private one resolves. Unresolved links still warn.
#![allow(rustdoc::private_intra_doc_links)]

pub mod api;
pub mod auth;
pub mod aws;
pub mod config;
pub(crate) mod csv;
pub mod error;
pub mod images;
pub(crate) mod object_view;
#[cfg(feature = "aws")]
pub mod pg;
pub(crate) mod qc;
pub mod register;
pub mod runs;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

use std::sync::Arc;

/// The lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Production authentication configuration. The verifier owns Cognito JWKS selection/refresh;
/// RBAC then maps groups, or applies the deployment's explicit authenticated-user default role.
pub struct AuthContext {
    pub verifier: auth::JwtVerifier,
    pub issuer: String,
    /// The set of accepted app-client ids (web + CLI). Fail-closed when empty: an empty set
    /// authorizes no token.
    pub audiences: Vec<String>,
    pub rbac: Arc<auth::rbac::Rbac>,
}

impl AuthContext {
    pub(crate) async fn verify(&self, token: &str) -> Result<auth::Claims, auth::AuthError> {
        self.verifier.verify(token, &self.issuer, &self.audiences).await
    }
}

/// The deploy-supplied configuration handlers read, composed once from the environment: submit,
/// registration, the task list and `/auth/config` all read it. What differs per project (bucket,
/// roles, data sources, upload and viewer flags) is [`AppState::projects`]. The menu is the source
/// of truth registration validates a workflow's queue-classes against; the coordinator queue is the
/// on-demand, never-spot queue a run's head job launches on, because a spot reclaim of the head job
/// would end the whole run.
#[derive(Clone, Debug, Default)]
pub struct DeployConfig {
    pub coordinator_queue: String,
    pub queue_classes: Vec<String>,
    /// The tenant ECR repository URI (`<id>.dkr.ecr.<region>.amazonaws.com/<repo>`) that holds
    /// workflow worker images. Every run pulls `{worker_image_repo}@{digest}`, so this is where a
    /// registered image actually lives — used to derive an ECR console link for a version even when
    /// the registration pinned only a bare digest (which carries no ECR coordinates of its own).
    pub worker_image_repo: String,
    /// The tenant ECR repository URI biffix builds workflow images into
    /// (`BIFFIX_BUILDS_IMAGE_REPO`): only biffix's own build and import projects can push to it, so
    /// it is the only place a built image is trusted from
    /// ([`crate::images::build::in_builds_repo`]). Startup refuses an empty value; only a config
    /// built in code (the tests' `Default`) can leave it empty, which trusts a built image from
    /// anywhere.
    pub builds_image_repo: String,
    /// Each Batch job queue's configured capacity, keyed by physical queue name: `spot` or
    /// `on_demand`. The fallback for a host's spot/on-demand label when EC2 never reported the
    /// instance's own lifecycle (from `BIFFIX_QUEUE_CAPACITIES`).
    pub queue_capacities: std::collections::HashMap<String, String>,
    /// Days after a run ends that its intermediate files are deleted (`BIFFIX_RETENTION_DAYS`);
    /// `None` keeps them forever. Shown by the launch form; the reconcile tick does the deleting.
    pub retention_days: Option<u32>,
    /// The public OAuth client configuration `GET /auth/config` serves, or why there is none.
    pub oauth_client: auth::oauth_client::OAuthClient,
}

/// Shared application state handed to axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn store::Store>,
    pub registry: Arc<dyn store::registry::RegistryStore>,
    pub events: Arc<dyn store::EventSink>,
    pub cloud: Arc<dyn aws::CloudProvider>,
    pub(crate) auth: Option<Arc<AuthContext>>,
    /// Verifies the per-run capability token on `POST /events`. `None` disables ingest entirely —
    /// the route fails closed rather than accepting unauthenticated appends.
    pub capability: Option<Arc<auth::capability::CapabilityVerifier>>,
    pub deploy: DeployConfig,
    /// The deploy's projects: each request picks its project's context from them
    /// ([`config::projects::Projects::for_version`], [`config::projects::Projects::for_run`]).
    pub projects: Arc<config::projects::ProjectsHandle>,
    pub settings: Arc<dyn store::settings::SettingsStore>,
    /// Reference bundles a launch form's reference dropdown offers.
    pub references: Arc<dyn store::references::ReferenceStore>,
    /// Experiments (named groups of a project's runs).
    pub experiments: Arc<dyn store::experiments::ExperimentStore>,
    /// Resolves an authenticated principal to a human label (email/name) at submit. Best-effort.
    pub(crate) identity: Arc<dyn auth::identity::IdentityResolver>,
}
