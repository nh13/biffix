//! A run as the API shows it: its detail ([`run_detail`]: the projection's run with the DAG
//! rollup, provenance, images and prefix facts filled in), a list's summaries with their lifecycle
//! and cost ([`with_list_facts`]), and its samples × steps ([`sample_grid_of`]).

use biffix_api::{
    DagNode, OutputPrefixKind, RunDag, RunDetail, RunPreparation, RunProvenance, RunSummary,
    RuntimeUse, SubmittedImage,
};
use biffix_core::event::{EventData, Kind, RunEvent, SubmitImage};
use biffix_core::ids::RunId;
use biffix_core::state::RunState;

use crate::config::queue_capacity::fill_queue_capacity;
use crate::store::{RunDetailView, StoreError};
use crate::AppState;

/// Build the DAG view: the node set is the union of the declared graph nodes and the groups tasks
/// actually rolled up onto (so a task whose rule never appeared in the graph still renders, and a
/// declared rule with no task instances still shows as an empty/pending node). Per-node counts come
/// from grouping the run's tasks by `group` and their state (cached is a subset of succeeded).
fn build_dag(d: &RunDetailView) -> RunDag {
    use std::collections::BTreeMap;
    let mut nodes: BTreeMap<String, DagNode> = BTreeMap::new();
    for id in &d.dag_nodes {
        nodes.entry(id.clone()).or_insert_with(|| DagNode { id: id.clone(), ..Default::default() });
    }
    let derived;
    let groups = if d.task_groups.is_empty() {
        derived = crate::store::task_group_counts(&d.tasks);
        &derived
    } else {
        &d.task_groups
    };
    for g in groups {
        let node = nodes
            .entry(g.group.clone())
            .or_insert_with(|| DagNode { id: g.group.clone(), ..Default::default() });
        let n = g.count;
        match g.state.parse::<biffix_core::state::TaskState>() {
            Ok(biffix_core::state::TaskState::Succeeded) => node.succeeded += n,
            Ok(biffix_core::state::TaskState::Running) => node.running += n,
            Ok(biffix_core::state::TaskState::Failed) => node.failed += n,
            // Queued (live attempts, none started yet) rolls up onto the node's pending count, as
            // do a task the run's cancel ended (it never finished) and any not-yet-classified
            // state.
            Ok(
                biffix_core::state::TaskState::Queued | biffix_core::state::TaskState::Cancelled,
            )
            | Err(_) => node.pending += n,
        }
        if g.cached {
            node.cached += n;
        }
    }
    // Keep only edges whose endpoints are known nodes (a dangling edge would draw to nothing).
    let edges = d
        .dag_edges
        .iter()
        .filter(|(f, t)| nodes.contains_key(f) && nodes.contains_key(t))
        .map(|(f, t)| [f.clone(), t.clone()])
        .collect();
    RunDag { nodes: nodes.into_values().collect(), edges }
}

/// Fill a summary's lifecycle facts, which the server decides so a client never re-derives them
/// from the open `state` string: `terminal` is [`RunState::is_terminal`] (an unknown state is not).
pub(crate) fn with_lifecycle(mut run: RunSummary) -> RunSummary {
    run.terminal = state_is_terminal(&run.state);
    run
}

/// Whether a run's (open) `state` string is a terminal [`RunState`]; an unknown state is not.
pub(crate) fn state_is_terminal(state: &str) -> bool {
    state.parse::<RunState>().is_ok_and(RunState::is_terminal)
}

/// How `prefix` relates to the run: its own per-run default, the per-run default of a run in the
/// resume chain it continues (the one it resumed, or that run's own origin: a resume keeps its
/// prior's prefix, so a resume of a resume is still in the chain's first run's), or a caller-chosen
/// one. Keyed on the object key (`biffix/runs/<id>/`), not the bucket, so it holds for any project
/// bucket; a caller can never choose a prefix under `biffix/runs/`
/// (`crate::runs::submit::output_prefix_override`), so another run's there is the chain's.
fn prefix_kind(prefix: &str, run_id: &RunId, resumed_from: Option<&RunId>) -> OutputPrefixKind {
    use biffix_core::s3_layout::{run_prefix, split_run_prefix, split_s3_uri};
    let key = split_s3_uri(prefix).map(|(_, key)| key);
    if key == Some(run_prefix(run_id.as_str()).as_str()) {
        OutputPrefixKind::Isolated
    } else if resumed_from.is_some() && split_run_prefix(prefix).is_some() {
        OutputPrefixKind::Resumed
    } else {
        OutputPrefixKind::Custom
    }
}

/// A Nextflow run's resume cache under its output prefix: the coordinator's
/// `cloudcache.path = <prefix>.nextflow/cache`, keyed by session
/// (docker/coordinator/lib/nextflow.sh), the prefix taken as recorded as the coordinator takes it
/// (a prefix recorded without its trailing `/` puts the cache at `<prefix>.nextflow/cache`).
fn nextflow_cache_uri(prefix: &str, session_id: &str) -> String {
    format!("{prefix}.nextflow/cache/{session_id}/")
}

impl From<RunDetailView> for RunDetail {
    fn from(d: RunDetailView) -> Self {
        let dag = build_dag(&d);
        // A bundle that no longer parses (recorded by an older contract) is left out, but logged.
        // Read as the event's type, then converted, so a field the contract adds reaches the API.
        let reference_bundle = d.reference_bundle.and_then(|b| {
            serde_json::from_value::<biffix_core::event::ReferenceUse>(b)
                .map(biffix_api::ReferenceUse::from)
                .inspect_err(|e| {
                    tracing::warn!(
                        run_id = %d.run.run_id,
                        error = %e,
                        "run detail: the recorded reference_bundle does not parse; leaving it out"
                    );
                })
                .ok()
        });
        RunDetail {
            cancelled_by: d.cancelled_by,
            summary: summary_with_cost(d.run, d.cost.as_ref()),
            tasks: d.tasks,
            params: d.params,
            // The effective prefix recorded at launch (none before the run launches).
            output_prefix: d.output_prefix,
            coordinator_job_id: d.coordinator_job_id,
            coordinator_log_stream: d.coordinator_log_stream,
            coordinator_host: d.coordinator_host,
            engine_session_id: d.engine_session_id,
            session_mismatch: d.session_mismatch,
            resumed_from: d.resumed_from,
            workflow_registered_at_micros: d.workflow_registered_at_micros,
            launched_at_micros: d.launched_at_micros,
            input_tables: d.input_tables,
            reference_bundle,
            retention: d.retention,
            work_deleted_at_micros: d.work_deleted_at_micros,
            cost: d.cost,
            provenance: None,
            submit_images: None,
            preparing: None,
            dag,
            // Filled by `run_detail`, which knows the deploy's bucket.
            resumable: false,
            resume_cache_uri: None,
            prefix_kind: None,
        }
    }
}

/// The provenance block of a `run_submitted` anchor; `None` for any other event.
fn provenance_of(ev: &RunEvent) -> Option<RunProvenance> {
    match &ev.data {
        EventData::RunSubmitted(anchor) => Some(RunProvenance {
            git_sha: anchor.git_sha.clone(),
            image_digest: anchor.image_digest.clone(),
            image_source_digest: anchor.image_source_digest.clone(),
            runtime: anchor.runtime.as_ref().map(RuntimeUse::from),
            image_map: anchor.image_map.clone(),
            submit_images: submit_image_views(anchor.submit_images.as_deref().unwrap_or_default()),
        }),
        _ => None,
    }
}

/// The images a run was given at submit, from its anchor or (a run that never launched) its
/// `run_preparing`; `None` for any other event, or when there were none.
fn submit_images_of(ev: &RunEvent) -> Option<Vec<SubmittedImage>> {
    match &ev.data {
        EventData::RunSubmitted(anchor) => {
            submit_image_views(anchor.submit_images.as_deref().unwrap_or_default())
        }
        EventData::RunPreparing(p) => submit_image_views(&p.images),
        _ => None,
    }
}

/// The images given at submit as the API shows them; `None` when there were none.
fn submit_image_views(images: &[SubmitImage]) -> Option<Vec<SubmittedImage>> {
    (!images.is_empty()).then(|| images.iter().map(SubmittedImage::from).collect())
}

/// Where a run waiting for its submit-time images stands (read-only): each image as the workflow
/// page shows it, and why the run cannot launch yet. `preparing` is the run's `run_preparing`.
async fn preparing_view(
    state: &AppState,
    preparing: &RunEvent,
) -> Result<Option<RunPreparation>, String> {
    let EventData::RunPreparing(p) = &preparing.data else {
        return Ok(None);
    };
    let rec =
        state.registry.get_version(&p.workflow_version_id).await.map_err(|e| e.to_string())?;
    let ctx =
        crate::images::VersionContext::of(state, &rec.version).await.map_err(|e| e.to_string())?;
    // Independent reads: concurrently, in the run's order.
    let ctx = &ctx;
    let statuses = futures::future::try_join_all(p.images.iter().map(|image| async move {
        crate::images::status(state, ctx, &crate::runs::launch::as_version_image(image)).await
    }))
    .await
    .map_err(|e| e.to_string())?;
    Ok(Some(RunPreparation {
        images: statuses.iter().map(crate::images::view::image_view).collect(),
        not_ready: statuses.iter().filter_map(crate::runs::launch::not_ready_reason).collect(),
        since_micros: preparing.ts.as_micros(),
    }))
}

/// Runs as a list serves them: each with its lifecycle and its cost as its run page has it
/// ([`summary_with_cost`]), the costs in one store read
/// ([`crate::store::cost::CostReads::run_costs`]), never one per run. The cost is a nicety: when
/// that read fails the runs are still served, without costs (logged).
pub(crate) async fn with_list_facts(state: &AppState, runs: Vec<RunSummary>) -> Vec<RunSummary> {
    let ids: Vec<RunId> = runs.iter().map(|r| r.run_id.clone()).collect();
    let mut costs = if ids.is_empty() {
        Default::default()
    } else {
        state.store.run_costs(&ids).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "run list: reading the page's run costs failed");
            Default::default()
        })
    };
    runs.into_iter()
        .map(|run| {
            let cost = costs.remove(run.run_id.as_str()).unwrap_or_default();
            with_lifecycle(with_cost(run, cost))
        })
        .collect()
}

/// A run's summary from its detail: with its lifecycle and its cost, as [`with_list_facts`] gives
/// both.
pub(crate) fn summary_with_cost(run: RunSummary, cost: Option<&biffix_api::RunCost>) -> RunSummary {
    with_lifecycle(with_cost(run, crate::store::cost::ListCost::from_run_cost(cost)))
}

fn with_cost(run: RunSummary, cost: crate::store::cost::ListCost) -> RunSummary {
    RunSummary { cost_usd: cost.usd, cost_pending: cost.pending, ..run }
}

/// A loaded run's samples × steps, from its sample facts, DAG and state: what
/// `GET /runs/{id}/samples` returns, and what a resume of the run carries of it.
pub(crate) async fn sample_grid_of(
    state: &AppState,
    run: &RunDetailView,
) -> Result<biffix_core::sample_progress::SampleGrid, StoreError> {
    let (spec, tasks) = state.store.sample_facts(&run.run.run_id).await?;
    Ok(grid(spec.as_ref(), &tasks, &run.dag_nodes, &run.dag_edges, &run.run.state))
}

/// A run's samples × steps from its sample facts and DAG, given its `state`.
pub(crate) fn grid_from_facts(
    facts: &crate::store::runs::RunSampleFacts,
    state: &str,
) -> biffix_core::sample_progress::SampleGrid {
    grid(facts.spec.as_ref(), &facts.tasks, &facts.dag_nodes, &facts.dag_edges, state)
}

/// A run's samples × steps from its sample spec, task facts and DAG, given its `state`.
fn grid(
    spec: Option<&biffix_core::event::SampleSpec>,
    tasks: &[biffix_core::sample_progress::TaskFact],
    dag_nodes: &[String],
    dag_edges: &[(String, String)],
    state: &str,
) -> biffix_core::sample_progress::SampleGrid {
    use biffix_core::event::SampleSpec;
    use biffix_core::sample_progress::{RunEnd, SampleGrid};
    use std::collections::BTreeSet;

    let nodes: BTreeSet<String> = dag_nodes.iter().cloned().collect();
    let edges: BTreeSet<(String, String)> = dag_edges.iter().cloned().collect();
    // An unreadable state is read as a live run: nothing is settled or stopped early.
    let end = state.parse::<RunState>().map_or(RunEnd::Live, RunEnd::of);
    let default_spec = SampleSpec::default();
    SampleGrid::build(spec.unwrap_or(&default_spec), tasks, (&nodes, &edges), end)
}

/// A loaded run as `GET /runs/{id}` serves it: its detail with what the projection does not hold
/// filled in (whether a submit can resume it, its submit provenance and images, where a run waiting
/// for its images stands, its output prefix and how it relates to the run, its Nextflow resume
/// cache, and each host's queue capacity). `with_tasks: false` leaves out the task table.
pub(crate) async fn run_detail(state: &AppState, d: RunDetailView, with_tasks: bool) -> RunDetail {
    let rid = d.run.run_id.clone();
    // The submit path's own resume check, so the page offers resume exactly when a submit takes it.
    let projects = state.projects.current().await;
    let resumable = crate::runs::submit::resume::resumable_now(&d, &projects).is_ok();
    let mut detail: RunDetail = d.into();
    detail.resumable = resumable;
    if !with_tasks {
        detail.tasks.clear();
    }
    // A display nicety: failing to read the run's anchor leaves the provenance block out rather than
    // failing the page. A run that never launched has no anchor (nor provenance): its
    // `run_preparing` says what images the submit gave.
    let preparing = detail.summary.state == RunState::PreparingImages.as_str();
    let accepted = match state.store.run_anchor(&rid).await {
        Ok(Some(anchor)) => Some(anchor),
        Ok(None) => state.store.run_event(&rid, &Kind::RunPreparing).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "get_run: reading the run's run_preparing failed");
            None
        }),
        Err(e) => {
            tracing::warn!(error = %e, "get_run: reading the run's anchor failed");
            None
        }
    };
    detail.provenance = accepted.as_ref().and_then(provenance_of);
    detail.submit_images = accepted.as_ref().and_then(submit_images_of);
    // Waiting for its images: where each one is (a nicety too).
    if preparing {
        if let Some(ev) = accepted.as_ref() {
            detail.preparing = preparing_view(state, ev).await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, "get_run: reading the run's images failed");
                None
            });
        }
    }
    if let Some(prefix) = detail.output_prefix.as_deref() {
        detail.prefix_kind = Some(prefix_kind(prefix, &rid, detail.resumed_from.as_ref()));
        if detail.summary.engine.as_deref()
            == Some(biffix_workflow::engines::Engine::Nextflow.as_str())
        {
            detail.resume_cache_uri = detail
                .engine_session_id
                .as_deref()
                .map(|session| nextflow_cache_uri(prefix, session));
        }
    }
    for task in &mut detail.tasks {
        if let Some(host) = task.host.as_mut() {
            fill_queue_capacity(host, &state.deploy.queue_capacities);
        }
    }
    if let Some(host) = detail.coordinator_host.as_mut() {
        // The coordinator always runs on the deploy's coordinator queue (configured as an ARN).
        let coordinator_queue = &state.deploy.coordinator_queue;
        let name = coordinator_queue.rsplit('/').next().unwrap_or(coordinator_queue);
        host.queue.get_or_insert_with(|| name.to_string());
        fill_queue_capacity(host, &state.deploy.queue_capacities);
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(state: &str) -> RunSummary {
        serde_json::from_value(serde_json::json!({
            "run_id": "r1", "state": state, "tasks_observed": 0, "tasks_succeeded": 0,
            "tasks_running": 0, "tasks_failed": 0, "tasks_cached": 0,
        }))
        .unwrap()
    }

    #[test]
    fn terminal_is_the_server_lifecycle_and_an_unknown_state_is_not_terminal() {
        for s in RunState::ALL {
            assert_eq!(with_lifecycle(summary(s.as_str())).terminal, s.is_terminal());
        }
        let mut stale = summary("succeeded");
        stale.state = "someday".into();
        stale.terminal = true;
        assert!(!with_lifecycle(stale).terminal);
    }

    #[test]
    fn a_prefix_is_isolated_resumed_or_custom_by_its_key() {
        let run = RunId::new("run-b");
        let prior = RunId::new("run-a");
        let kind = |p: &str, r: Option<&RunId>| prefix_kind(p, &run, r);
        assert_eq!(kind("s3://any/biffix/runs/run-b/", None), OutputPrefixKind::Isolated);
        assert_eq!(kind("s3://any/biffix/runs/run-a/", Some(&prior)), OutputPrefixKind::Resumed);
        // A resume of a resume keeps the chain's first run's prefix.
        assert_eq!(
            kind("s3://any/biffix/runs/run-origin/", Some(&prior)),
            OutputPrefixKind::Resumed
        );
        // Another run's per-run prefix without a resume link is caller-chosen.
        assert_eq!(kind("s3://any/biffix/runs/run-a/", None), OutputPrefixKind::Custom);
        // A suffix match under another key is not the run's own prefix.
        assert_eq!(kind("s3://any/mine/biffix/runs/run-b/", None), OutputPrefixKind::Custom);
        assert_eq!(kind("s3://any/shared/", Some(&prior)), OutputPrefixKind::Custom);
    }

    #[test]
    fn the_nextflow_cache_sits_under_the_prefix_by_session() {
        assert_eq!(
            nextflow_cache_uri("s3://b/biffix/runs/r1/", "s1"),
            "s3://b/biffix/runs/r1/.nextflow/cache/s1/"
        );
        // As the coordinator appends it: a prefix recorded without its slash is not a folder.
        assert_eq!(nextflow_cache_uri("s3://b/root", "s1"), "s3://b/root.nextflow/cache/s1/");
    }
}
