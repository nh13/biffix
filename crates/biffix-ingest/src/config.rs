//! The ingest Lambdas' configuration, each read once from the environment at cold start (through
//! [`biffix_core::env`], so both Lambdas, and the backend, parse a shape one way) and passed down:
//! [`IngestConfig`] for the SQS writer, [`ReconcileConfig`] for the scheduled tick.
//!
//! The variable names are a contract with the CDK constructs that set them
//! (`infra/biffix_infra/constructs/{ingest_pipeline,reconcile}.py`); `tests/it/config_env.rs`
//! holds each config to its construct.

use std::collections::BTreeMap;

use biffix_core::env::{ConfigErrors, DbConfig, Env, EnvError, Errors};

/// A config's startup errors, every one at once.
pub type ConfigError = ConfigErrors<EnvError>;

/// The coordinator queues (`BIFFIX_COORDINATOR_QUEUES`, comma-separated): a Batch job on one of
/// them is a run's coordinator, any other a worker task.
const COORDINATOR_QUEUES: &str = "BIFFIX_COORDINATOR_QUEUES";

/// The ingest (SQS writer) Lambda's configuration.
#[derive(Clone, Debug, Default)]
pub struct IngestConfig {
    /// The database (`DATABASE_URL` or `BIFFIX_DB_SECRET_ARN`).
    pub db: DbConfig,
    /// `BIFFIX_COORDINATOR_QUEUES`; empty when unset.
    pub coordinator_queues: Vec<String>,
}

impl IngestConfig {
    /// From the process environment.
    pub fn from_process_env() -> Result<Self, ConfigError> {
        Self::from_env(&Env::process())
    }

    /// From `env`.
    pub fn from_env(env: &Env) -> Result<Self, ConfigError> {
        let mut errors = Errors::default();
        let config = Self {
            db: errors.take(DbConfig::from_env(env)),
            coordinator_queues: env.list(COORDINATOR_QUEUES),
        };
        errors.finish(config)
    }
}

/// The reconcile (scheduled tick) Lambda's configuration.
#[derive(Clone, Debug, Default)]
pub struct ReconcileConfig {
    /// The database (`DATABASE_URL` or `BIFFIX_DB_SECRET_ARN`).
    pub db: DbConfig,
    /// The ingest queue the tick enqueues terminal facts on (`BIFFIX_INGEST_QUEUE_URL`, required).
    pub ingest_queue_url: String,
    /// `BIFFIX_COORDINATOR_QUEUES`; empty when unset.
    pub coordinator_queues: Vec<String>,
    /// The ingest dead-letter queue the status probe samples (`BIFFIX_INGEST_DLQ_URL`).
    pub ingest_dlq_url: Option<String>,
    /// The worker job queues the status probe health-checks (`BIFFIX_WORKER_QUEUES`).
    pub worker_queues: Vec<String>,
    /// The API's public readiness URL (`BIFFIX_API_READYZ_URL`); `None` leaves `api` unprobed.
    pub api_readyz_url: Option<String>,
    /// The name prefix of this deploy's CloudWatch alarms (`BIFFIX_ALARM_PREFIX`).
    pub alarm_prefix: Option<String>,
    /// The deploy's region (`AWS_REGION`): a host's region when its AZ is unknown, for pricing.
    pub deploy_region: Option<String>,
    /// Per Batch queue: its configured capacity and root volume, for pricing
    /// (`BIFFIX_QUEUE_PRICING`; empty when unset). A value that is not such a JSON object fails the
    /// cold start: a cost is written once, so pricing every job without its queue's volume and
    /// capacity would be wrong for good.
    pub queue_pricing: BTreeMap<String, QueuePricing>,
    /// Days after a run ends that its intermediate files are deleted (`BIFFIX_RETENTION_DAYS`, at
    /// most `u32::MAX`, as the backend reads it for the launch form); `None` keeps them forever.
    pub retention_days: Option<u64>,
    /// Days a Snakemake between-workflow cache entry is kept (`BIFFIX_SNAKEMAKE_CACHE_DAYS`);
    /// `None` keeps entries forever.
    pub cache_days: Option<u64>,
    /// The project buckets whose Snakemake cache the sweep expires (`BIFFIX_CACHE_BUCKETS`,
    /// comma-separated).
    pub cache_buckets: Vec<String>,
}

/// A queue's pricing-relevant configuration, from the deploy (`BIFFIX_QUEUE_PRICING`).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct QueuePricing {
    /// The fallback when a host's own lifecycle was not recorded.
    #[serde(default)]
    pub capacity: Option<Capacity>,
    #[serde(default)]
    pub volume: Option<crate::pricing::VolumeSpec>,
}

/// A queue's configured capacity (`spot` or `on_demand`, as `host_registry.lifecycle` spells a
/// host's own).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capacity {
    Spot,
    OnDemand,
}

impl Capacity {
    /// The `host_registry.lifecycle` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Capacity::Spot => "spot",
            Capacity::OnDemand => "on_demand",
        }
    }
}

/// The shape `BIFFIX_QUEUE_PRICING` takes, for its error.
const QUEUE_PRICING_SHAPE: &str =
    "an object of queue name -> {\"capacity\": \"spot\"|\"on_demand\", \
     \"volume\": {\"gb\", \"iops\", \"throughput\"}}";

impl ReconcileConfig {
    /// From the process environment.
    pub fn from_process_env() -> Result<Self, ConfigError> {
        Self::from_env(&Env::process())
    }

    /// From `env`.
    pub fn from_env(env: &Env) -> Result<Self, ConfigError> {
        let mut errors = Errors::default();
        let config = Self {
            db: errors.take(DbConfig::from_env(env)),
            ingest_queue_url: errors.take(env.required("BIFFIX_INGEST_QUEUE_URL")),
            coordinator_queues: env.list(COORDINATOR_QUEUES),
            ingest_dlq_url: env.string("BIFFIX_INGEST_DLQ_URL"),
            worker_queues: env.list("BIFFIX_WORKER_QUEUES"),
            api_readyz_url: env.string("BIFFIX_API_READYZ_URL"),
            alarm_prefix: env.string("BIFFIX_ALARM_PREFIX"),
            deploy_region: env.string("AWS_REGION"),
            queue_pricing: errors
                .take(env.json("BIFFIX_QUEUE_PRICING", QUEUE_PRICING_SHAPE))
                .unwrap_or_default(),
            retention_days: errors.take(env.u32("BIFFIX_RETENTION_DAYS")).map(u64::from),
            cache_days: errors.take(env.u64("BIFFIX_SNAKEMAKE_CACHE_DAYS")),
            cache_buckets: env.list("BIFFIX_CACHE_BUCKETS"),
        };
        errors.finish(config)
    }
}

/// What one invoke of the reconcile Lambda asks for, from its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileInvoke {
    /// The scheduled tick (an EventBridge `Scheduled Event`, or any payload with no `action`).
    Tick,
    /// `{"action": "refold", "after": "<run id>", "limit": <n>}`: re-fold one page of runs in
    /// run-id order after `after` (`biffix_db::projection::refold_runs`), the replay that applies
    /// a reducer fix or backfills a new projection column. Pass back the answer's `last_run_id` as
    /// `after` until it is `null`.
    Refold { after: Option<String>, limit: i64 },
}

/// A refold page's default size.
pub const REFOLD_PAGE_DEFAULT: i64 = 25;

impl ReconcileInvoke {
    /// Read an invoke payload. A payload with no `action` is the scheduled tick; any other action,
    /// or a refold field of the wrong type or out of range, is an error naming it, so a mistyped
    /// manual invoke never runs something else.
    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, String> {
        use biffix_db::projection::REFOLD_PAGE_MAX;
        let field = |name: &str| payload.get(name).filter(|value| !value.is_null());
        let Some(action) = field("action") else {
            return Ok(ReconcileInvoke::Tick);
        };
        if action.as_str() != Some("refold") {
            return Err(format!("unknown action {action} (expected \"refold\")"));
        }
        let after = field("after")
            .map(|after| {
                after
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("`after` must be a run id string, not {after}"))
            })
            .transpose()?;
        let limit = match field("limit") {
            None => REFOLD_PAGE_DEFAULT,
            Some(limit) => {
                limit.as_i64().filter(|n| (1..=REFOLD_PAGE_MAX).contains(n)).ok_or_else(|| {
                    format!("`limit` must be an integer from 1 to {REFOLD_PAGE_MAX}, not {limit}")
                })?
            }
        };
        Ok(ReconcileInvoke::Refold { after, limit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    #[test]
    fn an_invoke_payload_is_the_tick_or_a_well_formed_refold() {
        use serde_json::json;
        let read = |payload: serde_json::Value| ReconcileInvoke::from_payload(&payload);
        // EventBridge's scheduled event carries no `action`.
        let scheduled = json!({"version": "0", "detail-type": "Scheduled Event", "detail": {}});
        assert_eq!(read(scheduled), Ok(ReconcileInvoke::Tick));
        assert_eq!(read(json!({})), Ok(ReconcileInvoke::Tick));
        assert_eq!(
            read(json!({"action": "refold"})),
            Ok(ReconcileInvoke::Refold { after: None, limit: REFOLD_PAGE_DEFAULT })
        );
        assert_eq!(
            read(json!({"action": "refold", "after": "run-9", "limit": 200})),
            Ok(ReconcileInvoke::Refold { after: Some("run-9".into()), limit: 200 })
        );
        for bad in [
            json!({"action": "Refold"}),
            json!({"action": 1}),
            json!({"action": "refold", "after": 123}),
            json!({"action": "refold", "limit": "50"}),
            json!({"action": "refold", "limit": 0}),
            json!({"action": "refold", "limit": 201}),
        ] {
            assert!(read(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_reconcile_config_keeps_its_defaults_and_reports_every_error() {
        let config = ReconcileConfig::from_env(&env(&[
            ("BIFFIX_DB_SECRET_ARN", "arn:secret"),
            ("BIFFIX_INGEST_QUEUE_URL", "https://sqs/q"),
            ("BIFFIX_COORDINATOR_QUEUES", "c1, c2,"),
            ("BIFFIX_RETENTION_DAYS", " 30 "),
            ("BIFFIX_QUEUE_PRICING", " "),
        ]))
        .unwrap();
        assert_eq!(config.db, DbConfig::Secret("arn:secret".into()));
        assert_eq!(config.coordinator_queues, ["c1", "c2"]);
        assert_eq!(config.retention_days, Some(30));
        assert!(config.queue_pricing.is_empty());
        assert_eq!(config.ingest_dlq_url, None);

        let errors =
            ReconcileConfig::from_env(&env(&[("BIFFIX_RETENTION_DAYS", "thirty")])).unwrap_err();
        let vars: BTreeSet<&str> = errors.0.iter().map(EnvError::var).collect();
        assert_eq!(
            vars,
            BTreeSet::from(["DATABASE_URL", "BIFFIX_INGEST_QUEUE_URL", "BIFFIX_RETENTION_DAYS"])
        );
        // The one set but malformed is invalid, not missing.
        let invalid: BTreeSet<&str> = errors
            .0
            .iter()
            .filter(|e| matches!(e, EnvError::Invalid { .. }))
            .map(EnvError::var)
            .collect();
        assert_eq!(invalid, BTreeSet::from(["BIFFIX_RETENTION_DAYS"]));
    }

    #[test]
    fn queue_pricing_parses_per_queue() {
        let config = ReconcileConfig::from_env(&env(&[
            ("DATABASE_URL", "postgres://x"),
            ("BIFFIX_INGEST_QUEUE_URL", "q"),
            ("BIFFIX_QUEUE_PRICING", r#"{"q-spot":{"capacity":"spot","volume":{"gb":100}}}"#),
        ]))
        .unwrap();
        let q = &config.queue_pricing["q-spot"];
        assert_eq!(q.capacity, Some(Capacity::Spot));
        assert_eq!(q.volume.as_ref().map(|v| v.gb), Some(100.0));
    }

    /// The backend reads `BIFFIX_RETENTION_DAYS` as 32-bit, so a larger value would make the two
    /// read one deploy setting differently.
    #[test]
    fn an_oversized_retention_fails_the_cold_start() {
        let errors = ReconcileConfig::from_env(&env(&[
            ("DATABASE_URL", "postgres://x"),
            ("BIFFIX_INGEST_QUEUE_URL", "q"),
            ("BIFFIX_RETENTION_DAYS", "4294967296"),
        ]))
        .unwrap_err();
        let vars: BTreeSet<&str> = errors.0.iter().map(EnvError::var).collect();
        assert_eq!(vars, BTreeSet::from(["BIFFIX_RETENTION_DAYS"]));
        let config = ReconcileConfig::from_env(&env(&[
            ("DATABASE_URL", "postgres://x"),
            ("BIFFIX_INGEST_QUEUE_URL", "q"),
            ("BIFFIX_RETENTION_DAYS", "4294967295"),
        ]))
        .unwrap();
        assert_eq!(config.retention_days, Some(u64::from(u32::MAX)));
    }

    /// A malformed value would price every job without its queue's volume or capacity, and a cost
    /// is written once, so it fails the cold start like the backend's `BIFFIX_QUEUE_CAPACITIES`.
    #[test]
    fn malformed_queue_pricing_fails_the_cold_start() {
        for value in [
            "not json",
            r#"["q-spot"]"#,
            r#"{"q-spot":{"capacity":"Spot"}}"#,
            r#"{"q-spot":{"volume":{"iops":3000}}}"#,
        ] {
            let errors = ReconcileConfig::from_env(&env(&[
                ("DATABASE_URL", "postgres://x"),
                ("BIFFIX_INGEST_QUEUE_URL", "q"),
                ("BIFFIX_QUEUE_PRICING", value),
            ]))
            .unwrap_err();
            let vars: Vec<&str> = errors.0.iter().map(EnvError::var).collect();
            assert_eq!(vars, ["BIFFIX_QUEUE_PRICING"], "{value}");
        }
    }

    #[test]
    fn the_ingest_config_needs_only_the_database() {
        let config = IngestConfig::from_env(&env(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert!(config.coordinator_queues.is_empty());
        assert!(IngestConfig::from_env(&env(&[])).is_err());
    }
}
