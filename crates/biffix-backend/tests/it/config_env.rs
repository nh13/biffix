//! The backend Lambdas' environment contract: the variables each entry point's config reads
//! (listed here, the reference for the deploy docs) against what the CDK sets on its Lambda. Every
//! variable the CDK sets must be read, and every one the binary requires must be set.

use std::collections::BTreeSet;

use biffix_backend::auth::AuthModeError;
use biffix_backend::config::env::{ApiConfig, BackendConfig, ConfigError, StartupErrors};
use biffix_core::env::{Env, EnvError};
use biffix_core::testing::cdk::{assert_contract, block, env_names};

const API_STACK: &str = include_str!("../../../../infra/biffix_infra/stacks/api_stack.py");
const BACKEND_API: &str = include_str!("../../../../infra/biffix_infra/constructs/backend_api.py");
const BACKEND_SWEEP: &str =
    include_str!("../../../../infra/biffix_infra/constructs/backend_sweep.py");
const CONTROL_PLANE: &str =
    include_str!("../../../../infra/biffix_infra/constructs/control_plane.py");
const WIRING: &str = include_str!("../../../../infra/biffix_infra/wiring.py");

/// What every backend entry point reads.
const BACKEND_VARS: &[&str] = &[
    "AWS_DEFAULT_REGION",
    "AWS_REGION",
    "BIFFIX_ALERT_TOPIC_ARN",
    "BIFFIX_BUILDS_IMAGE_REPO",
    "BIFFIX_CAPABILITY_SECRET_ARN",
    "BIFFIX_CLI_CLIENT_ID",
    "BIFFIX_CLI_REDIRECT_URIS",
    "BIFFIX_COORDINATOR_IMAGE",
    "BIFFIX_COORDINATOR_QUEUE",
    "BIFFIX_DB_SECRET_ARN",
    "BIFFIX_DEFAULT_PROJECT",
    "BIFFIX_IMAGE_BUILD_PROJECT_AMD64",
    "BIFFIX_IMAGE_BUILD_PROJECT_ARM64",
    "BIFFIX_IMAGE_IMPORT_PROJECT",
    "BIFFIX_INGEST_URL",
    "BIFFIX_OAUTH_AUTHORIZATION_ENDPOINT",
    "BIFFIX_OAUTH_LOGOUT_ENDPOINT",
    "BIFFIX_OAUTH_LOGOUT_URI",
    "BIFFIX_OAUTH_REDIRECT_URI",
    "BIFFIX_OAUTH_TOKEN_ENDPOINT",
    "BIFFIX_OIDC_AUDIENCE",
    "BIFFIX_OIDC_ISSUER",
    "BIFFIX_PROJECTS_FILE",
    "BIFFIX_PROJECTS_PATH",
    "BIFFIX_QUEUE_CAPACITIES",
    "BIFFIX_QUEUE_CLASSES",
    "BIFFIX_RETENTION_DAYS",
    "BIFFIX_RUNTIME_BUILD_PROJECT",
    "BIFFIX_VARIANT_JOB_DEFINITION_PREFIX",
    "BIFFIX_WORKER_IMAGE_REPO",
    "DATABASE_URL",
];

/// What an HTTP entry point reads besides.
const API_ONLY_VARS: &[&str] =
    &["BIFFIX_AUTHENTICATED_DEFAULT_ROLE", "BIFFIX_AUTH_DISABLED", "BIFFIX_GROUP_ROLE_MAP"];

fn names(lists: &[&[&str]]) -> BTreeSet<String> {
    lists.iter().flat_map(|list| list.iter().map(|&name| name.to_owned())).collect()
}

/// The variables a config read, and what it requires, from `pairs` (one name, or names one of
/// which suffices, per error).
fn probe<T: std::fmt::Debug>(
    read: impl Fn(&Env) -> Result<T, StartupErrors>,
    pairs: &[(&str, &str)],
) -> (BTreeSet<String>, Vec<Vec<String>>) {
    let env = Env::from_pairs(pairs.iter().copied());
    let errors = read(&env).expect_err("a config with nothing set");
    let required = errors
        .0
        .iter()
        .filter_map(|error| match error {
            ConfigError::Env(EnvError::Missing { var }) => Some(vec![var.clone()]),
            ConfigError::Env(EnvError::MissingOneOf { vars, .. }) => Some(vars.clone()),
            ConfigError::Auth(AuthModeError::Unauthenticated) => {
                Some(vec!["BIFFIX_OIDC_ISSUER".to_owned()])
            }
            _ => None,
        })
        .collect();
    (env.read(), required)
}

/// What a config reads and requires: with nothing set, and with the deploy's SSM projects path
/// set (which requires its default project).
fn contract<T: std::fmt::Debug>(
    read: impl Fn(&Env) -> Result<T, StartupErrors> + Copy,
) -> (BTreeSet<String>, Vec<Vec<String>>) {
    let (names, mut required) = probe(read, &[]);
    let (_, with_path) = probe(read, &[("BIFFIX_PROJECTS_PATH", "/p")]);
    required.extend(with_path);
    (names, required)
}

/// The environment the CDK sets on both backend Lambdas: the database, the projects, the
/// coordinator-variant builds (and their build projects) and the launch grants.
fn control_plane_env() -> BTreeSet<String> {
    let mut set = env_names(WIRING);
    set.extend(env_names(CONTROL_PLANE));
    set.extend(env_names(block(API_STACK, "build_project_names={")));
    set
}

#[test]
fn the_api_lambda_reads_what_the_cdk_sets_it() {
    let (read, required) = contract(ApiConfig::from_env);
    assert_eq!(read, names(&[BACKEND_VARS, API_ONLY_VARS]));
    let mut set = control_plane_env();
    set.extend(env_names(BACKEND_API));
    set.extend(env_names(block(API_STACK, "def _configure_backend(")));
    set.extend(env_names(block(API_STACK, "def _wire_alerts(")));
    assert_contract("API", &set, &read, &required);
    // The text checks found what they should have.
    for name in [
        "BIFFIX_OIDC_ISSUER",
        "BIFFIX_GROUP_ROLE_MAP",
        "BIFFIX_ALERT_TOPIC_ARN",
        "BIFFIX_IMAGE_BUILD_PROJECT_*",
    ] {
        assert!(set.contains(name), "{name}: {set:?}");
    }
}

#[test]
fn the_sweep_lambda_reads_what_the_cdk_sets_it() {
    let (read, required) = contract(BackendConfig::from_env);
    assert_eq!(read, names(&[BACKEND_VARS]));
    let mut set = control_plane_env();
    set.extend(env_names(BACKEND_SWEEP));
    assert_contract("sweep", &set, &read, &required);
    assert!(set.contains("BIFFIX_DB_SECRET_ARN"), "{set:?}");
}
