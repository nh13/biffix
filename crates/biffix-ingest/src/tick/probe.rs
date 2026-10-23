//! The status-probe step: gather the `/status` page's health signals and write one
//! `status_samples` row per component every tick that runs in full (every 5 minutes while a
//! coordinator is active, hourly on an idle tenant). Every AWS and database call that backs the page
//! lives here, not in the frequently polled `/status` handler, which reads only Postgres (the latest
//! sample per component and a couple of live queries). Each sample promises the next one within the
//! time its tick's reason gives (`biffix_db::idle::Why::next_full_within_secs`): a sample *missing*
//! past that promise is how the handler detects a dead probe. The classifiers are `crate::status`'s;
//! the queries `crate::store::status`'s.

use biffix_db::idle::Why;

use crate::status::{
    classify_alarms, classify_api, classify_batch_signals, classify_ingest_signals, first_missing,
    ApiProbe, Health,
};
use crate::store::status::{record_samples, stuck_run_count, unfolded_age_secs};

use super::State;

/// How long the API readiness probe waits before calling the API unreachable.
const API_PROBE_TIMEOUT_SECS: u64 = 10;

/// Gather every signal and write one `status_samples` row per component at a common timestamp,
/// promising the next within what `why`, the reason this tick runs, gives, then prune samples past
/// the retention window. Best-effort: a failure in one source degrades only
/// its component, naming the signal it could not read; the write itself is what advances history.
pub(super) async fn sample(state: &State, why: Why) -> Result<usize, lambda_runtime::Error> {
    // Postgres-derived signals. A query error becomes "unknown" (`None`), which the classifiers
    // report as Degraded naming the signal, never as the healthy value it would otherwise alias
    // (`None` unfolded age means "caught up", and `0` means "no stuck runs").
    let unfolded = unfolded_age_secs(&state.pool)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "probe: unfolded_age_secs query failed"))
        .ok();
    let stuck = stuck_run_count(&state.pool)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "probe: stuck_run_count query failed"))
        .ok();
    // AWS-derived signals (best-effort; `None` = could not be read). The ingest queue's name is
    // its URL's last path segment.
    let ingest_queue_name = state.config.ingest_queue_url.rsplit('/').next();
    let dlq = dlq_depth(&state.sqs, state.config.ingest_dlq_url.as_deref()).await;
    let queue_age = ingest_queue_age_secs(&state.cloudwatch, ingest_queue_name).await;
    let (api_h, api_d) = classify_api(&probe_api(state.config.api_readyz_url.as_deref()).await);
    let coordinator_problem =
        queue_problem(&state.batch, &state.queue_context.coordinator_queues).await;
    let worker_problem = queue_problem(&state.batch, &state.config.worker_queues).await;

    let alarms = match &state.config.alarm_prefix {
        Some(prefix) => match firing_alarms(&state.cloudwatch, prefix).await {
            Some(firing) => classify_alarms(&firing),
            None => (Health::Degraded, Some("could not read the alarms".into())),
        },
        None => (Health::Ok, Some("not watched (no alarm prefix configured)".into())),
    };

    let (ingest_h, ingest_d) = classify_ingest_signals(unfolded, dlq, queue_age);
    let (batch_h, batch_d) = classify_batch_signals(coordinator_problem, worker_problem, stuck);

    // `database` is trivially ok (we hold a live connection); `reconciler` is ok by virtue of this
    // probe running. The handler downgrades any component whose latest sample is stale, and marks
    // `reconciler` down outright, so these are not dishonest greens — they mean "the probe observed
    // the infra alive at this timestamp". `api` is probed for real, through its public URL.
    let rows: [(&str, Health, Option<String>); 6] = [
        ("api", api_h, api_d),
        ("database", Health::Ok, None),
        ("event_ingest", ingest_h, ingest_d),
        ("batch_compute", batch_h, batch_d),
        ("reconciler", Health::Ok, None),
        ("alarms", alarms.0, alarms.1),
    ];
    record_samples(&state.pool, &rows, why.next_full_within_secs())
        .await
        .map(|()| 0)
        .map_err(lambda_runtime::Error::from)
}

/// The deploy's alarms now in ALARM, as `(name, description)`; `None` when they could not be read.
async fn firing_alarms(
    cloudwatch: &aws_sdk_cloudwatch::Client,
    prefix: &str,
) -> Option<Vec<(String, Option<String>)>> {
    use aws_sdk_cloudwatch::types::StateValue;
    let mut firing = Vec::new();
    let mut pages = cloudwatch
        .describe_alarms()
        .alarm_name_prefix(prefix)
        .state_value(StateValue::Alarm)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        match page {
            Ok(out) => firing.extend(out.metric_alarms().iter().map(|a| {
                (
                    a.alarm_name().unwrap_or_default().to_string(),
                    a.alarm_description().map(str::to_string),
                )
            })),
            Err(e) => {
                tracing::warn!(
                    error = %aws_sdk_cloudwatch::error::DisplayErrorContext(&e),
                    "probe: describe alarms failed (swallowed)"
                );
                return None;
            }
        }
    }
    Some(firing)
}

/// Seconds the ingest queue's oldest message has waited, from the queue's CloudWatch
/// `ApproximateAgeOfOldestMessage` metric (SQS exposes it only there, not via GetQueueAttributes):
/// the latest 1-minute maximum over the last 10 minutes. `Some(0)` when the queue is empty (no
/// datapoints) or no queue is configured; `None` when the metric could not be read, so the caller
/// can report the signal as unknown rather than healthy.
async fn ingest_queue_age_secs(
    cloudwatch: &aws_sdk_cloudwatch::Client,
    queue_name: Option<&str>,
) -> Option<i64> {
    use aws_sdk_cloudwatch::primitives::DateTime;
    use aws_sdk_cloudwatch::types::{Dimension, Statistic};
    let Some(queue_name) = queue_name else {
        return Some(0);
    };
    let now = std::time::SystemTime::now();
    let start = now - std::time::Duration::from_secs(600);
    let result = cloudwatch
        .get_metric_statistics()
        .namespace("AWS/SQS")
        .metric_name("ApproximateAgeOfOldestMessage")
        .dimensions(Dimension::builder().name("QueueName").value(queue_name).build())
        .start_time(DateTime::from(start))
        .end_time(DateTime::from(now))
        .period(60)
        .statistics(Statistic::Maximum)
        .send()
        .await;
    match result {
        Ok(out) => Some(
            out.datapoints()
                .iter()
                .filter_map(|d| Some((d.timestamp()?.secs(), d.maximum()?)))
                .max_by_key(|(ts, _)| *ts)
                .map_or(0, |(_, age)| age as i64),
        ),
        Err(e) => {
            tracing::warn!(
                error = %aws_sdk_cloudwatch::error::DisplayErrorContext(&e),
                "probe: ingest queue age metric failed"
            );
            None
        }
    }
}

/// GET the API's readiness URL through its public front door.
async fn probe_api(url: Option<&str>) -> ApiProbe {
    let Some(url) = url else {
        return ApiProbe::NotConfigured;
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(API_PROBE_TIMEOUT_SECS))
        .build()
    {
        Ok(client) => client,
        Err(e) => return ApiProbe::Unreachable(e.to_string()),
    };
    match client.get(url).send().await {
        Ok(resp) => ApiProbe::Status(resp.status().as_u16()),
        Err(e) if e.is_timeout() => {
            ApiProbe::Unreachable(format!("no answer within {API_PROBE_TIMEOUT_SECS}s"))
        }
        Err(e) => ApiProbe::Unreachable(e.to_string()),
    }
}

/// Dead-letter-queue visible-message count. `Some(0)` when no DLQ is configured; `None` when the
/// queue attributes could not be read or the count does not parse, so the caller can report the
/// signal as unknown rather than as an empty DLQ.
async fn dlq_depth(sqs: &aws_sdk_sqs::Client, dlq_url: Option<&str>) -> Option<i64> {
    let Some(url) = dlq_url else { return Some(0) };
    match sqs
        .get_queue_attributes()
        .queue_url(url)
        .attribute_names(aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages)
        .send()
        .await
    {
        Ok(out) => {
            let depth = out
                .attributes()
                .and_then(|a| {
                    a.get(&aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages)
                })
                .and_then(|v| v.parse::<i64>().ok());
            if depth.is_none() {
                tracing::warn!("probe: DLQ GetQueueAttributes returned no parseable message count");
            }
            depth
        }
        Err(e) => {
            tracing::warn!(
                error = %aws_sdk_sqs::error::DisplayErrorContext(&e),
                "probe: DLQ GetQueueAttributes failed"
            );
            None
        }
    }
}

/// What is wrong with the named job queues or their compute environments (each must be
/// ENABLED+VALID), or `None` when nothing is. An empty name list is trivially healthy. A describe
/// error is a problem too: an unknown Batch state should surface, not read as healthy.
async fn queue_problem(batch: &aws_sdk_batch::Client, names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    // Every page: each describe answers at most 100 a call, and a name left for a later page would
    // otherwise read as missing.
    let mut queues = Vec::new();
    let mut pages =
        batch.describe_job_queues().set_job_queues(Some(names.to_vec())).into_paginator().send();
    while let Some(page) = pages.next().await {
        match page {
            Ok(out) => queues.extend(out.job_queues.unwrap_or_default()),
            Err(e) => return Some(format!("could not describe job queues: {e}")),
        }
    }
    // DescribeJobQueues silently OMITS a requested name that does not exist (returns success with a
    // shorter list), so a deleted/misnamed queue would otherwise read healthy — the exact failure
    // this probe exists to catch. Require every requested name to come back.
    let returned: std::collections::HashSet<&str> =
        queues.iter().flat_map(|q| [q.job_queue_name(), q.job_queue_arn()]).flatten().collect();
    if let Some(detail) = first_missing(names, &returned, "job queue") {
        return Some(detail);
    }
    let mut ce_names: Vec<String> = Vec::new();
    for q in &queues {
        let enabled = q.state() == Some(&aws_sdk_batch::types::JqState::Enabled);
        let valid = q.status() == Some(&aws_sdk_batch::types::JqStatus::Valid);
        if !enabled || !valid {
            return Some(format!(
                "job queue {} is {:?}/{:?}",
                q.job_queue_name().unwrap_or("?"),
                q.state(),
                q.status()
            ));
        }
        for ce in q.compute_environment_order() {
            if let Some(name) = ce.compute_environment() {
                ce_names.push(name.to_string());
            }
        }
    }
    if ce_names.is_empty() {
        return None;
    }
    let mut environments = Vec::new();
    let mut pages = batch
        .describe_compute_environments()
        .set_compute_environments(Some(ce_names.clone()))
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        match page {
            Ok(out) => environments.extend(out.compute_environments.unwrap_or_default()),
            Err(e) => return Some(format!("could not describe compute environments: {e}")),
        }
    }
    // Same omit-on-missing behavior as DescribeJobQueues: a compute environment absent from the
    // response does not exist, which is DOWN — nothing can launch — not healthy.
    let returned: std::collections::HashSet<&str> = environments
        .iter()
        .flat_map(|ce| [ce.compute_environment_name(), ce.compute_environment_arn()])
        .flatten()
        .collect();
    if let Some(detail) = first_missing(&ce_names, &returned, "compute environment") {
        return Some(detail);
    }
    for ce in &environments {
        let enabled = ce.state() == Some(&aws_sdk_batch::types::CeState::Enabled);
        let valid = ce.status() == Some(&aws_sdk_batch::types::CeStatus::Valid);
        if !enabled || !valid {
            return Some(format!(
                "compute environment {} is {:?}/{:?}{}",
                ce.compute_environment_name().unwrap_or("?"),
                ce.state(),
                ce.status(),
                ce.status_reason().map(|r| format!(" — {r}")).unwrap_or_default()
            ));
        }
    }
    None
}
