//! The ingest Lambdas' environment contract: the variables each Lambda's config reads (listed
//! here, the reference for the deploy docs) against what the CDK sets on it. Every variable the CDK
//! sets must be read, and every one the binary requires must be set.

use std::collections::BTreeSet;

use biffix_core::env::{ConfigErrors, Env, EnvError};
use biffix_core::testing::cdk::{assert_contract, env_names};
use biffix_ingest::config::{IngestConfig, ReconcileConfig};

const INGEST_PIPELINE: &str =
    include_str!("../../../../infra/biffix_infra/constructs/ingest_pipeline.py");
const RECONCILE: &str = include_str!("../../../../infra/biffix_infra/constructs/reconcile.py");
const WIRING: &str = include_str!("../../../../infra/biffix_infra/wiring.py");

const DB_VARS: &[&str] = &["BIFFIX_DB_SECRET_ARN", "DATABASE_URL"];

/// What the ingest (SQS writer) Lambda reads besides the database.
const INGEST_VARS: &[&str] = &["BIFFIX_COORDINATOR_QUEUES"];

/// What the reconcile Lambda reads besides the database.
const RECONCILE_VARS: &[&str] = &[
    "AWS_REGION",
    "BIFFIX_ALARM_PREFIX",
    "BIFFIX_API_READYZ_URL",
    "BIFFIX_CACHE_BUCKETS",
    "BIFFIX_COORDINATOR_QUEUES",
    "BIFFIX_INGEST_DLQ_URL",
    "BIFFIX_INGEST_QUEUE_URL",
    "BIFFIX_QUEUE_PRICING",
    "BIFFIX_RETENTION_DAYS",
    "BIFFIX_SNAKEMAKE_CACHE_DAYS",
    "BIFFIX_WORKER_QUEUES",
];

fn names(lists: &[&[&str]]) -> BTreeSet<String> {
    lists.iter().flat_map(|list| list.iter().map(|&name| name.to_owned())).collect()
}

/// The variables a config reads, and what it requires (one name, or names one of which suffices,
/// per error), from an empty environment.
fn contract<T: std::fmt::Debug>(
    read: impl Fn(&Env) -> Result<T, ConfigErrors<EnvError>>,
) -> (BTreeSet<String>, Vec<Vec<String>>) {
    let env = Env::from_pairs(std::iter::empty::<(String, String)>());
    let errors = read(&env).expect_err("a config with nothing set");
    let required = errors
        .0
        .iter()
        .filter_map(|error| match error {
            EnvError::Missing { var } => Some(vec![var.clone()]),
            EnvError::MissingOneOf { vars, .. } => Some(vars.clone()),
            _ => None,
        })
        .collect();
    (env.read(), required)
}

#[test]
fn the_ingest_lambda_reads_what_the_cdk_sets_it() {
    let (read, required) = contract(IngestConfig::from_env);
    assert_eq!(read, names(&[DB_VARS, INGEST_VARS]));
    let mut set = env_names(WIRING);
    set.extend(env_names(INGEST_PIPELINE));
    assert_contract("ingest", &set, &read, &required);
    assert!(set.contains("BIFFIX_COORDINATOR_QUEUES"), "{set:?}");
}

#[test]
fn the_reconcile_lambda_reads_what_the_cdk_sets_it() {
    let (read, required) = contract(ReconcileConfig::from_env);
    assert_eq!(read, names(&[DB_VARS, RECONCILE_VARS]));
    let mut set = env_names(WIRING);
    set.extend(env_names(RECONCILE));
    assert_contract("reconcile", &set, &read, &required);
    assert!(required.iter().flatten().any(|v| v == "BIFFIX_INGEST_QUEUE_URL"));
}
