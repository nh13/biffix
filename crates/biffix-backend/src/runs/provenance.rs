//! A run's provenance as a Workflow Run RO-Crate (WRROC 0.6, the Provenance Run Crate profile),
//! derived from the event log: the run's `run_submitted` anchor (git sha, image digest, image map,
//! input versions), its projection (tasks, attempts, outputs) and the workflow version it ran. One
//! implementation serves both engines, so a Snakemake and a Nextflow run describe themselves the
//! same way.
//!
//! The crate is *detached*: it holds only `ro-crate-metadata.json`, and its data entities are the
//! run's S3 objects, identified by their `s3://` URIs. Nothing is copied.
//!
//! The mapping (see `docs/provenance.md`):
//! - the workflow (`ComputationalWorkflow`) is the version's entrypoint at its git sha;
//! - each rule/process (a DAG group) is a tool (`SoftwareApplication`) and a step (`HowToStep`);
//! - each task is a tool execution (`CreateAction`) and a step execution (`ControlAction`);
//! - the engine run is an `OrganizeAction` whose result is the workflow run (`CreateAction`);
//! - inputs (with their S3 version at submit), params, reference-bundle files and samplesheets are
//!   the run's `object`; published/output files are its `result`.
//!
//! Local `@id`s live in namespaces a [`slug`] cannot reach: `#<kind>/<slug>` (and
//! `#usage/<task slug>/<metric>`), and `slug` always encodes `/`, so no two entities share an id.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde_json::{json, Map, Value};

use biffix_api::Task;
use biffix_core::event::{EventData, RunEvent};
use biffix_core::ids::RunId;

use crate::register::workflow::WorkflowSource;
use crate::store::outputs::{OutputQuery, OutputRow};
use crate::store::registry::RegisteredVersion;
use crate::store::{RunDetailView, StoreError};
use biffix_workflow::engines::Engine;

/// How long a run's log must be quiet before its S3 copy is written, so a late reconcile or a
/// straggling engine event lands in it rather than in a rewrite.
pub(crate) const SETTLE_SECS: i64 = 600;
/// Crates written per sweep, bounding one invocation's work (and the backfill after a deploy).
pub(crate) const WRITES_PER_SWEEP: i64 = 25;
/// The most outputs a crate lists; a crate that lists fewer than the run has says so.
pub(crate) const MAX_OUTPUTS: i64 = 100_000;
/// The file name of an RO-Crate's metadata.
pub(crate) const METADATA_FILE: &str = "ro-crate-metadata.json";

/// Where a run's crate is written: its own prefix, never the (possibly shared) output prefix.
pub fn crate_uri(bucket: &str, run_id: &str) -> Option<String> {
    (!bucket.is_empty()).then(|| {
        format!(
            "s3://{bucket}/{}provenance/{METADATA_FILE}",
            crate::runs::output_prefix::run_output_prefix(run_id)
        )
    })
}

/// Build the run's crate, from its full projection `detail` (every task), the registry and the log.
pub(crate) async fn run_crate(
    state: &crate::AppState,
    run_id: &RunId,
    detail: RunDetailView,
) -> Result<Value, StoreError> {
    // A version the registry no longer has degrades the crate; any other registry error fails the
    // build, so the sweep retries it rather than persisting a crate with no workflow identity.
    let version = match &detail.run.workflow {
        Some(v) => crate::store::found(state.registry.get_version(v.as_str()).await)?,
        None => None,
    };
    let anchor = state.store.run_anchor(run_id).await?.and_then(AnchorFacts::from_event);
    // The run's experiment, named when it still exists (an experiment is never deleted, so a miss
    // keeps only the id the run's log records).
    let experiment = match &detail.run.experiment_id {
        Some(id) => Some(ExperimentRef {
            id: id.clone(),
            name: crate::store::found(state.experiments.get(id).await)?.map(|e| e.name),
        }),
        None => None,
    };
    // The run's own files are in its project's bucket.
    let projects = state.projects.current().await;
    let bucket = &projects.for_run(&detail).bucket;
    let (outputs, total) = run_outputs(state, &detail).await?;
    let truncated = total > outputs.len() as i64;
    if truncated {
        tracing::warn!(
            run = %run_id,
            listed = outputs.len(),
            total,
            "provenance: the crate lists only the first {MAX_OUTPUTS} outputs"
        );
    }
    // The coordinator uploads the events at its exit: a run that never launched has none.
    let launched = detail.launched_at_micros.is_some();
    let events_uri = (!bucket.is_empty() && launched).then(|| {
        format!(
            "s3://{bucket}/{}events.jsonl",
            crate::runs::output_prefix::run_output_prefix(run_id.as_str())
        )
    });
    Ok(build_crate(&ProvenanceInput {
        detail: &detail,
        version: version.as_ref(),
        anchor: anchor.as_ref(),
        outputs: &outputs,
        outputs_total: truncated.then_some(total),
        events_uri: events_uri.as_deref(),
        generated_at_micros: biffix_core::clock::epoch_micros(),
        biffix_version: env!("CARGO_PKG_VERSION"),
        experiment: experiment.as_ref(),
    }))
}

/// The run's outputs of every role (at most [`MAX_OUTPUTS`]), and how many it has in all: as the
/// outputs API lists them (`crate::runs::outputs::output_scope`), so a resumed run's include the
/// files it reused from the runs it resumed (each row naming the run that registered it). Two
/// reads, every role but intermediates first, then the intermediates with the room left: so the
/// published files and outputs fill the [`MAX_OUTPUTS`] cap before any intermediate does.
async fn run_outputs(
    state: &crate::AppState,
    detail: &RunDetailView,
) -> Result<(Vec<OutputRow>, i64), StoreError> {
    let scope = crate::runs::outputs::output_scope(state, detail).await?;
    let query = |roles: Option<Vec<String>>, limit: i64| OutputQuery {
        task: None,
        roles,
        uri_prefix: None,
        after: None,
        limit,
        checkable_bucket: None,
    };
    let rest = state.store.list_outputs_in(&scope, &query(None, MAX_OUTPUTS)).await?;
    let room = MAX_OUTPUTS - rest.rows.len() as i64;
    let intermediate = state
        .store
        .list_outputs_in(&scope, &query(Some(vec!["intermediate".into()]), room))
        .await?;
    let total = rest.total + intermediate.total;
    let mut rows = rest.rows;
    rows.extend(intermediate.rows);
    Ok((rows, total))
}

/// Write the crates that are due into their runs' S3 prefixes (in each run's project bucket);
/// returns how many were written. One run's failure (reading the run, building, writing or marking
/// its crate, or a project with no bucket) is logged, recorded and skipped, so it cannot block the
/// rest: the run is due again on the next sweep, then less often each time it fails again, and
/// after the runs that have not failed ([`ProvenanceStore::mark_provenance_failed`]), so a run that
/// keeps failing never holds a fresh run's place.
///
/// [`ProvenanceStore::mark_provenance_failed`]: crate::store::provenance::ProvenanceStore::mark_provenance_failed
pub async fn write_due(state: &crate::AppState) -> Result<usize, StoreError> {
    let projects = state.projects.current().await;
    if projects.iter().all(|p| p.bucket.is_empty()) {
        return Ok(0);
    }
    let due = state.store.provenance_due(SETTLE_SECS, WRITES_PER_SWEEP).await?;
    let mut written = 0;
    for (run_id, seq) in due {
        match write_one(state, &projects, &run_id, seq).await {
            Ok(()) => written += 1,
            Err(why) => {
                tracing::warn!(run = %run_id, error = %why, "provenance: could not write the crate");
                let now = biffix_core::clock::epoch_micros();
                if let Err(e) = state.store.mark_provenance_failed(&run_id, now).await {
                    tracing::warn!(run = %run_id, error = %e, "provenance: could not record the failure");
                }
            }
        }
    }
    Ok(written)
}

/// Build, write and mark run `run_id`'s crate (up to log `seq`); `Err` says which step failed.
async fn write_one(
    state: &crate::AppState,
    projects: &crate::config::projects::Projects,
    run_id: &RunId,
    seq: i64,
) -> Result<(), String> {
    let detail = state.store.get_run(run_id).await.map_err(|e| format!("reading the run: {e}"))?;
    let uri = crate_uri(&projects.for_run(&detail).bucket, run_id.as_str())
        .ok_or("its project has no bucket")?;
    let ro_crate =
        run_crate(state, run_id, detail).await.map_err(|e| format!("building it: {e}"))?;
    let body = serde_json::to_vec_pretty(&ro_crate).map_err(|e| format!("serializing it: {e}"))?;
    state
        .cloud
        .put_object(&uri, body, "application/ld+json")
        .await
        .map_err(|e| format!("writing it: {e}"))?;
    state
        .store
        .mark_provenance(run_id, seq, &uri, biffix_core::clock::epoch_micros())
        .await
        .map_err(|e| format!("marking it written: {e}"))
}

/// The RO-Crate and WRROC versions this crate conforms to.
pub(crate) const RO_CRATE: &str = "https://w3id.org/ro/crate/1.3";
const PROFILES: [&str; 4] = [
    "https://w3id.org/ro/wfrun/process/0.6",
    "https://w3id.org/ro/wfrun/workflow/0.6",
    "https://w3id.org/ro/wfrun/provenance/0.6",
    "https://w3id.org/workflowhub/workflow-ro-crate/1.1",
];
/// Where biffix's own `propertyID`s (resource usage, cache hits) are described.
const TERMS: &str = "https://github.com/nh13/biffix/blob/main/docs/provenance.md#";
/// The `@id` of the image the run's tasks ran in by default.
const DEFAULT_IMAGE_ID: &str = "#image";
/// The `@id` of the image the default image was derived from.
const SOURCE_IMAGE_ID: &str = "#image-source";

/// The provenance facts only the run's `run_submitted` anchor records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnchorFacts {
    pub git_sha: String,
    /// The digest of the image the run's tasks ran in by default (the derived image, for a
    /// Snakemake image the engine runtime was added to).
    pub image_digest: String,
    /// The digest `image_digest` was derived from (as imported or built), when it was derived.
    pub image_source_digest: Option<String>,
    /// The run's image map: each image reference its tasks may name, to the tenant image run.
    pub image_map: BTreeMap<String, String>,
    /// Per input URI: its S3 version id (else ETag) when the run was submitted.
    pub input_provenance: BTreeMap<String, String>,
}

impl AnchorFacts {
    /// The facts of a `run_submitted` event; `None` for any other event.
    pub fn from_event(ev: RunEvent) -> Option<Self> {
        match ev.data {
            EventData::RunSubmitted(anchor) => Some(Self {
                git_sha: anchor.git_sha,
                image_digest: anchor.image_digest,
                image_source_digest: anchor.image_source_digest,
                image_map: anchor.image_map.unwrap_or_default(),
                input_provenance: anchor.input_provenance.unwrap_or_default(),
            }),
            _ => None,
        }
    }
}

/// The experiment a run is in, as its crate names it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExperimentRef {
    pub id: String,
    /// Its name, when the experiment could be read.
    pub name: Option<String>,
}

/// Everything a crate is built from. Params in `detail` are already secret-redacted, so a secret
/// never reaches the crate.
pub struct ProvenanceInput<'a> {
    pub detail: &'a RunDetailView,
    pub version: Option<&'a RegisteredVersion>,
    pub anchor: Option<&'a AnchorFacts>,
    /// The run's outputs (every role; intermediates are listed as their task's results), those it
    /// reused from a run it resumed included (their `run_id` that run's).
    pub outputs: &'a [OutputRow],
    /// How many outputs the run has, when `outputs` lists fewer (it was capped); `None` when
    /// `outputs` is all of them.
    pub outputs_total: Option<i64>,
    /// The run's archived engine event stream (`…/events.jsonl`), when known.
    pub events_uri: Option<&'a str>,
    /// When the crate is generated (epoch micros).
    pub generated_at_micros: i64,
    /// The biffix release that generated it.
    pub biffix_version: &'a str,
    /// The experiment the run is in, if any: the root dataset is part of it.
    pub experiment: Option<&'a ExperimentRef>,
}

/// The run's `ro-crate-metadata.json`.
pub fn build_crate(input: &ProvenanceInput<'_>) -> Value {
    let mut graph = Graph::default();
    let detail = input.detail;
    let run = &detail.run;
    let run_id = run.run_id.as_str();
    let engine = input
        .version
        .map(|v| v.version.engine)
        .or_else(|| run.engine.as_deref().and_then(|e| e.parse::<Engine>().ok()));

    // ── the workflow, its tools and steps ──
    let workflow_id = workflow_id(input);
    let groups = step_order(detail, engine);
    let tool_id = |g: &str| format!("#tool/{}", slug(g));
    let step_id = |g: &str| format!("#step/{}", slug(g));
    // A rule/process is versioned with its workflow: the commit it ran at.
    let workflow_sha = input
        .version
        .map(|v| v.version.git_sha.as_str())
        .or_else(|| input.anchor.map(|a| a.git_sha.as_str()))
        .filter(|sha| !sha.is_empty());
    for (position, group) in groups.iter().enumerate() {
        let mut tool = json!({
            "@id": tool_id(group),
            "@type": "SoftwareApplication",
            "name": group,
        });
        if let Some(sha) = workflow_sha {
            tool["softwareVersion"] = json!(sha);
        }
        graph.add(tool);
        graph.add(json!({
            "@id": step_id(group),
            "@type": "HowToStep",
            "name": group,
            "position": position.to_string(),
            "workExample": {"@id": tool_id(group)},
        }));
    }

    // ── the images tasks ran in ──
    let mut images = container_images(input);
    // A task names the image its engine reported for it (`task_submitted.image`); a task with none
    // recorded (an older plugin) names the default image only when it is the run's one image.
    let single_image = images.run.first().filter(|_| images.run.len() == 1).cloned();
    let task_images: HashMap<&str, Value> = detail
        .tasks
        .iter()
        .filter_map(|t| t.image.as_deref())
        .map(|image| (image, images.reference(image)))
        .collect();
    for entity in std::mem::take(&mut images.entities) {
        graph.add(entity);
    }

    // ── inputs, reference-bundle files, samplesheets and params: the run's `object` ──
    let mut object: Vec<Value> = Vec::new();
    let anchor = input.anchor;
    for uri in &run.input_uris {
        let mut file = file_entity(uri);
        if let Some(tag) = anchor.and_then(|a| a.input_provenance.get(uri)) {
            if !tag.is_empty() {
                file.insert("version".into(), json!(tag));
            }
        }
        graph.add_file(file);
        object.push(json!({"@id": uri}));
    }
    if let Some(Value::Object(bundle)) = &detail.reference_bundle {
        let bundle_id = bundle.get("id").and_then(Value::as_str).unwrap_or("");
        if let Some(Value::Object(uris)) = bundle.get("uris") {
            for (key, uri) in uris {
                let Some(uri) = uri.as_str() else { continue };
                let mut file = file_entity(uri);
                file.insert(
                    "description".into(),
                    json!(format!("Reference bundle `{bundle_id}`: {key}")),
                );
                graph.add_file(file);
                object.push(json!({"@id": uri}));
            }
        }
    }
    if let Some(Value::Object(tables)) = &detail.input_tables {
        for (param, table) in tables {
            let Some(uri) = table.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let mut file = file_entity(uri);
            file.insert(
                "description".into(),
                json!(format!("Samplesheet biffix wrote for `{param}`")),
            );
            if let Some(etag) = table.get("etag").and_then(Value::as_str) {
                file.insert("version".into(), json!(etag));
            }
            graph.add_file(file);
            object.push(json!({"@id": uri}));
        }
    }
    // A param filled by input files is realized by those files (each `exampleOfWork` of it); any
    // other param by a `PropertyValue`.
    let input_files: HashSet<String> =
        object.iter().filter_map(|r| r["@id"].as_str().map(str::to_string)).collect();
    let mut formal: Vec<Value> = Vec::new();
    if let Value::Object(params) = &detail.params {
        for (name, value) in params {
            let formal_id = format!("#param/{}", slug(name));
            formal.push(json!({"@id": formal_id}));
            let mut parameter = json!({
                "@id": formal_id,
                "@type": "FormalParameter",
                "name": name,
            });
            match file_values(value).filter(|uris| uris.iter().all(|u| input_files.contains(*u))) {
                Some(uris) => {
                    let dirs = uris.iter().all(|u| u.ends_with('/'));
                    parameter["additionalType"] = json!(if dirs { "Dataset" } else { "File" });
                    if value.is_array() {
                        parameter["multipleValues"] = json!(true);
                    }
                    for uri in uris {
                        graph.link_example(uri, &formal_id);
                        object.push(json!({"@id": uri}));
                    }
                }
                None => {
                    parameter["additionalType"] = json!(data_type(value));
                    let value_id = format!("#param-value/{}", slug(name));
                    graph.add(json!({
                        "@id": value_id,
                        "@type": "PropertyValue",
                        "name": name,
                        "value": property_value(value),
                        "exampleOfWork": {"@id": formal_id},
                    }));
                    object.push(json!({"@id": value_id}));
                }
            }
            graph.add(parameter);
        }
    }
    dedup_ids(&mut object);

    // ── outputs: per task, and the run's published results ──
    let mut by_task: HashMap<&str, Vec<Value>> = HashMap::new();
    let mut result: Vec<Value> = Vec::new();
    for out in input.outputs {
        let mut file = file_entity(&out.s3_uri);
        if let Some(size) = out.size.or_else(|| out.meta.as_ref().and_then(|m| m.size)) {
            file.insert("contentSize".into(), json!(size.to_string()));
        }
        if let Some(etag) =
            out.etag.as_deref().or_else(|| out.meta.as_ref().and_then(|m| m.etag.as_deref()))
        {
            file.insert("version".into(), json!(etag));
        }
        // A file reused from a run this one resumed: a result of this run (as the outputs API
        // lists it), but made by that run's task, so no task of this run claims it.
        let reused_from = (out.run_id != run.run_id).then_some(&out.run_id);
        if let Some(origin) = reused_from {
            file.insert(
                "description".into(),
                json!(format!(
                    "Reused from run {origin}, which this run resumed: made there, not by this \
                     run's tasks."
                )),
            );
        }
        graph.add_file(file);
        if let (Some(task), None) = (out.task_key.as_deref(), reused_from) {
            by_task.entry(task).or_default().push(json!({"@id": out.s3_uri}));
        }
        if out.role == "published" || out.role == "output" {
            result.push(json!({"@id": out.s3_uri}));
        }
    }
    dedup_ids(&mut result);

    // ── who ran it ──
    let agent = run.submitter.as_deref().map(|submitter| {
        let id = format!("#agent/{}", slug(submitter));
        graph.add(json!({
            "@id": id,
            "@type": "Person",
            "name": run.submitter_label.as_deref().unwrap_or(submitter),
            "identifier": submitter,
        }));
        json!({"@id": id})
    });

    // ── task executions ──
    let mut control_actions: Vec<Value> = Vec::new();
    let mut task_actions: Vec<Value> = Vec::new();
    for task in &detail.tasks {
        let key = task.task_key.as_str();
        let group = task_group(task, engine);
        let action_id = format!("#task/{}", slug(key));
        let mut action = Map::new();
        action.insert("@id".into(), json!(action_id));
        action.insert("@type".into(), json!("CreateAction"));
        action.insert("name".into(), json!(key));
        action.insert("description".into(), json!(format!("Run of {group} for task {key}")));
        action.insert("instrument".into(), json!({"@id": tool_id(&group)}));
        if let Some(agent) = &agent {
            action.insert("agent".into(), agent.clone());
        }
        insert_time(&mut action, "startTime", task.started_at);
        insert_time(&mut action, "endTime", task.finished_at);
        action.insert("actionStatus".into(), json!(action_status(&task.state)));
        if let Some(error) = task_error(task) {
            action.insert("error".into(), json!(error));
        }
        if let Some(outputs) = by_task.get(key) {
            action.insert("result".into(), json!(outputs));
        }
        let image = match task.image.as_deref() {
            Some(reported) => task_images.get(reported).cloned(),
            None => single_image.clone(),
        };
        if let Some(image) = image {
            action.insert("containerImage".into(), image);
        }
        let usage = task_usage(key, task);
        if !usage.is_empty() {
            action.insert(
                "resourceUsage".into(),
                json!(usage.iter().map(|u| json!({"@id": u["@id"]})).collect::<Vec<_>>()),
            );
            for u in usage {
                graph.add(u);
            }
        }
        graph.add(Value::Object(action));
        task_actions.push(json!({"@id": action_id}));

        let control_id = format!("#step-run/{}", slug(key));
        graph.add(json!({
            "@id": control_id,
            "@type": "ControlAction",
            "name": format!("Run step {group} ({key})"),
            "instrument": {"@id": step_id(&group)},
            "object": {"@id": action_id},
            "actionStatus": action_status(&task.state),
        }));
        control_actions.push(json!({"@id": control_id}));
    }

    // ── the workflow run and the engine run ──
    let run_action_id = format!("#run-{}", slug(run_id));
    let mut run_action = Map::new();
    run_action.insert("@id".into(), json!(run_action_id));
    run_action.insert("@type".into(), json!("CreateAction"));
    run_action.insert(
        "name".into(),
        json!(format!("Run of {} ({run_id})", run.workflow_name.as_deref().unwrap_or("workflow"))),
    );
    run_action.insert("identifier".into(), json!(run_id));
    run_action.insert(
        "description".into(),
        json!(format!("biffix run {run_id}, submitted through biffix")),
    );
    run_action.insert("instrument".into(), json!({"@id": workflow_id}));
    insert_time(&mut run_action, "startTime", run.submitted_at);
    insert_time(&mut run_action, "endTime", run.completed_at);
    run_action.insert("actionStatus".into(), json!(action_status(&run.state)));
    if let Some(agent) = &agent {
        run_action.insert("agent".into(), agent.clone());
    }
    run_action.insert("object".into(), json!(object));
    run_action.insert("result".into(), json!(result));
    match images.run.as_slice() {
        [] => {}
        [one] => {
            run_action.insert("containerImage".into(), one.clone());
        }
        many => {
            run_action.insert("containerImage".into(), json!(many));
        }
    }
    graph.add(Value::Object(run_action));

    let engine_name = engine.map_or("workflow engine", crate::runs::submit::resume::engine_label);
    let mut engine_app = json!({
        "@id": "#engine",
        "@type": "SoftwareApplication",
        "name": engine_name,
    });
    if let Some(v) = input.version.map(|v| v.version.engine_version.as_str()) {
        if !v.is_empty() {
            engine_app["softwareVersion"] = json!(v);
        }
    }
    graph.add(engine_app);
    let mut engine_run = json!({
        "@id": "#engine-run",
        "@type": "OrganizeAction",
        "name": format!("Run of {engine_name} by biffix"),
        "instrument": {"@id": "#engine"},
        "object": control_actions,
        "result": {"@id": run_action_id},
        "actionStatus": action_status(&run.state),
    });
    if let Some(agent) = agent {
        engine_run["agent"] = agent;
    }
    graph.add(engine_run);

    // ── the workflow itself ──
    let mut workflow = Map::new();
    workflow.insert("@id".into(), json!(workflow_id));
    workflow.insert(
        "@type".into(),
        json!(["File", "SoftwareSourceCode", "ComputationalWorkflow", "HowTo"]),
    );
    workflow.insert("name".into(), json!(run.workflow_name.as_deref().unwrap_or("workflow")));
    if let Some(lang) = engine.map(language_entity) {
        workflow.insert("programmingLanguage".into(), json!({"@id": lang["@id"]}));
        graph.add(lang);
    }
    if let Some(v) = input.version {
        workflow.insert("identifier".into(), json!(v.version.id));
        if !v.version.git_sha.is_empty() {
            workflow.insert("version".into(), json!(v.version.git_sha));
        }
        if let WorkflowSource::Git { repo, .. } = &v.source {
            workflow.insert("codeRepository".into(), json!(repo));
        }
    } else if let Some(a) = anchor.filter(|a| !a.git_sha.is_empty()) {
        workflow.insert("version".into(), json!(a.git_sha));
    }
    workflow.insert("input".into(), json!(formal));
    workflow.insert(
        "hasPart".into(),
        json!(groups.iter().map(|g| json!({"@id": tool_id(g)})).collect::<Vec<_>>()),
    );
    workflow.insert(
        "step".into(),
        json!(groups.iter().map(|g| json!({"@id": step_id(g)})).collect::<Vec<_>>()),
    );
    graph.add(Value::Object(workflow));

    // ── the root dataset ──
    let mut has_part: Vec<Value> = vec![json!({"@id": workflow_id})];
    has_part.extend(graph.files.iter().map(|id| json!({"@id": id})));
    if let Some(events) = input.events_uri {
        let mut file = file_entity(events);
        file.insert(
            "description".into(),
            json!("The engine's event stream for this run, as biffix archived it"),
        );
        file.insert("encodingFormat".into(), json!("application/jsonl"));
        graph.add_file(file);
        has_part.push(json!({"@id": events}));
    }
    dedup_ids(&mut has_part);
    let name = run.run_name.as_deref().unwrap_or(run_id);
    let mut mentions = vec![json!({"@id": run_action_id}), json!({"@id": "#engine-run"})];
    mentions.extend(task_actions);
    mentions.extend(control_actions);
    let mut description = format!(
        "Workflow Run RO-Crate for biffix run {run_id}, generated by biffix {} from the run's \
         event log.",
        input.biffix_version
    );
    if let Some(total) = input.outputs_total {
        description.push_str(&format!(
            " The run has {total} outputs; this crate lists only {} of them.",
            input.outputs.len()
        ));
    }
    let mut root = json!({
        "@id": "./",
        "@type": "Dataset",
        "name": format!("Provenance of biffix run {name}"),
        "description": description,
        "datePublished": iso8601(input.generated_at_micros),
        "conformsTo": PROFILES.iter().map(|p| json!({"@id": p})).collect::<Vec<_>>(),
        "mainEntity": {"@id": workflow_id},
        "hasPart": has_part,
        "license": {"@id": "#license"},
        "mentions": mentions,
    });
    if let Some(experiment) = input.experiment {
        let id = format!("#experiment/{}", slug(&experiment.id));
        let mut entity = json!({
            "@id": id,
            "@type": "CreativeWork",
            "identifier": experiment.id,
            "description": "The biffix experiment this run is in",
        });
        if let Some(name) = &experiment.name {
            entity["name"] = json!(name);
        }
        graph.add(entity);
        root["isPartOf"] = json!({"@id": id});
    }
    graph.add(json!({
        "@id": "#license",
        "@type": "CreativeWork",
        "name": "Not specified",
        "description": "biffix records no license for a run's data or metadata; ask the run's \
                        owner before reusing it.",
    }));
    let descriptor = json!({
        "@id": "ro-crate-metadata.json",
        "@type": "CreativeWork",
        "about": {"@id": "./"},
        "conformsTo": {"@id": RO_CRATE},
    });
    for profile in PROFILES {
        graph.add(json!({"@id": profile, "@type": "CreativeWork", "name": profile_name(profile)}));
    }

    let mut entities = vec![descriptor, root];
    entities.extend(graph.entities);
    json!({
        "@context": [
            format!("{RO_CRATE}/context"),
            "https://w3id.org/ro/terms/workflow-run/context",
        ],
        "@graph": entities,
    })
}

/// The entities of a crate under construction, each `@id` once (first write wins, but a data
/// entity seen again gains the properties it lacked), in insertion order, plus the ids of its
/// data (file) entities for the root's `hasPart`.
#[derive(Default)]
struct Graph {
    entities: Vec<Value>,
    index: HashMap<String, usize>,
    files: Vec<String>,
}

impl Graph {
    fn add(&mut self, entity: Value) {
        let id = entity["@id"].as_str().unwrap_or_default().to_string();
        if let std::collections::hash_map::Entry::Vacant(slot) = self.index.entry(id) {
            slot.insert(self.entities.len());
            self.entities.push(entity);
        }
    }

    fn add_file(&mut self, file: Map<String, Value>) {
        let id = file["@id"].as_str().unwrap_or_default().to_string();
        match self.index.get(&id) {
            Some(&at) => {
                if let Value::Object(existing) = &mut self.entities[at] {
                    for (key, value) in file {
                        existing.entry(key).or_insert(value);
                    }
                }
            }
            None => {
                self.files.push(id);
                self.add(Value::Object(file));
            }
        }
    }

    /// Record that the data entity `id` realizes the formal parameter `formal_id`.
    fn link_example(&mut self, id: &str, formal_id: &str) {
        let Some(&at) = self.index.get(id) else {
            return;
        };
        let Value::Object(entity) = &mut self.entities[at] else {
            return;
        };
        let link = json!({"@id": formal_id});
        match entity.get_mut("exampleOfWork") {
            None => {
                entity.insert("exampleOfWork".into(), link);
            }
            Some(Value::Array(links)) => {
                if !links.contains(&link) {
                    links.push(link);
                }
            }
            Some(one) => {
                if *one != link {
                    *one = json!([one.clone(), link]);
                }
            }
        }
    }
}

/// A `File` data entity for an S3 URI, named by its last path segment. A prefix (ending in `/`) is
/// a `Dataset`.
fn file_entity(uri: &str) -> Map<String, Value> {
    let is_dir = uri.ends_with('/');
    let name = biffix_workflow::text::basename(uri.trim_end_matches('/')).to_string();
    let mut m = Map::new();
    m.insert("@id".into(), json!(uri));
    m.insert("@type".into(), json!(if is_dir { "Dataset" } else { "File" }));
    m.insert("name".into(), json!(name));
    m
}

/// The workflow's `@id`: a browsable URL of the entrypoint at the pinned commit for a GitHub
/// source, else `<source>#<entrypoint>`; `#workflow` when the version is unknown.
fn workflow_id(input: &ProvenanceInput<'_>) -> String {
    let Some(v) = input.version else {
        return "#workflow".into();
    };
    let entry = v.version.entrypoint.trim_start_matches("./");
    match &v.source {
        WorkflowSource::Git { repo, .. } => {
            let base = repo.trim_end_matches('/').trim_end_matches(".git");
            if base.starts_with("https://github.com/") && !v.version.git_sha.is_empty() {
                format!("{base}/blob/{}/{entry}", v.version.git_sha)
            } else {
                format!("{base}#{entry}")
            }
        }
        WorkflowSource::S3 { uri, .. } => format!("{}#{entry}", uri.trim_end_matches('/')),
    }
}

/// The run's `ContainerImage`s: the entities, and the references the run action names (the
/// default image first).
struct Images {
    entities: Vec<Value>,
    run: Vec<Value>,
    /// The default image's digest (`sha256:…`), when known.
    default_digest: Option<String>,
}

impl Images {
    /// The `{"@id": …}` of the entity for `image`, a reference as an engine reported it for a task
    /// (`<registry>/<repo>@sha256:…`, or a tag): the default image when it has the default's
    /// digest, else the image-map entity of the same reference, else a new entity added to the
    /// crate (and to the run's images: the run ran it). A reference with no digest is named, not
    /// hashed.
    fn reference(&mut self, image: &str) -> Value {
        let parts = parse_image(image);
        if let (Some(p), Some(default)) = (&parts, &self.default_digest) {
            if p.digest == *default {
                return json!({"@id": DEFAULT_IMAGE_ID});
            }
        }
        let id = format!("#image/{}", slug(image));
        if !self.entities.iter().any(|e| e["@id"] == id.as_str()) {
            let entity = match &parts {
                Some(p) => image_entity(&id, &p.digest, p.registry.as_deref(), p.name.as_deref()),
                None => json!({
                    "@id": id,
                    "@type": "ContainerImage",
                    "additionalType": {"@id": "https://w3id.org/ro/terms/workflow-run#DockerImage"},
                    "name": image,
                }),
            };
            self.entities.push(entity);
            self.run.push(json!({"@id": id}));
        }
        json!({"@id": id})
    }
}

/// The images the run's tasks ran in. The default is the anchor's digest (what the run
/// launched; for a Snakemake image, the derived one), else the version's; it `isBasedOn` the
/// image it was derived from. Every other image in the run's image map is listed too.
fn container_images(input: &ProvenanceInput<'_>) -> Images {
    let mut images = Images { entities: Vec::new(), run: Vec::new(), default_digest: None };
    let present = |d: &str| -> Option<String> { (!d.is_empty()).then(|| d.to_string()) };
    let version_digest = input.version.and_then(|v| present(&v.version.image_digest));
    let map_default = input
        .anchor
        .and_then(|a| a.image_map.get(crate::images::map::DEFAULT_KEY))
        .and_then(|image| parse_image(image));
    let digest = input
        .anchor
        .and_then(|a| present(&a.image_digest))
        .or_else(|| version_digest.clone())
        .or_else(|| map_default.as_ref().map(|p| p.digest.clone()));
    if let Some(digest) = &digest {
        // Where it lives: the run's worker image when that is this digest, else the version's
        // recorded repository when this is the version's own image.
        let (registry, name) = match &map_default {
            Some(p) if p.digest == *digest => (p.registry.clone(), p.name.clone()),
            _ => version_coords(input, digest),
        };
        let mut image =
            image_entity(DEFAULT_IMAGE_ID, digest, registry.as_deref(), name.as_deref());
        let source = input
            .anchor
            .and_then(|a| a.image_source_digest.as_deref())
            .and_then(present)
            .filter(|s| s != digest);
        if let Some(source) = source {
            let (registry, name) = version_coords(input, &source);
            images.entities.push(image_entity(
                SOURCE_IMAGE_ID,
                &source,
                registry.as_deref(),
                name.as_deref(),
            ));
            image["isBasedOn"] = json!({"@id": SOURCE_IMAGE_ID});
        }
        images.entities.push(image);
        images.run.push(json!({"@id": DEFAULT_IMAGE_ID}));
    }
    let others: BTreeSet<&str> = input
        .anchor
        .map(|a| a.image_map.values().map(String::as_str).collect())
        .unwrap_or_default();
    for other in others {
        let Some(parts) = parse_image(other) else {
            continue;
        };
        if digest.as_deref() == Some(parts.digest.as_str()) {
            continue;
        }
        let id = format!("#image/{}", slug(other));
        images.entities.push(image_entity(
            &id,
            &parts.digest,
            parts.registry.as_deref(),
            parts.name.as_deref(),
        ));
        images.run.push(json!({"@id": id}));
    }
    images.default_digest = digest;
    images
}

/// The version's recorded ECR registry host and repository, when `digest` is the version's own
/// image (a derived image may live elsewhere, so it is not assumed to).
fn version_coords(input: &ProvenanceInput<'_>, digest: &str) -> (Option<String>, Option<String>) {
    let Some(v) = input.version.filter(|v| v.version.image_digest == digest) else {
        return (None, None);
    };
    match crate::runs::submit::worker_image_repo_uri(&v.version)
        .as_deref()
        .and_then(|uri| uri.split_once('/'))
    {
        Some((registry, repo)) => (Some(registry.to_string()), Some(repo.to_string())),
        None => (None, v.version.image_repository.clone()),
    }
}

/// A pinned image reference (`<registry>/<name>@sha256:<hex>`), split.
struct ImageParts {
    registry: Option<String>,
    /// The repository, without a tag.
    name: Option<String>,
    digest: String,
}

/// `image` split into its registry, repository and digest, when it is pinned to a sha256 digest:
/// by the repo's Docker-grammar parser ([`biffix_workflow::images::ImageRef`]), as the image map
/// spells it, so one reference splits one way everywhere (a tag before the digest is not part of
/// the name; `localhost` is a registry). A reference an engine reported that the parser refuses is
/// split leniently: the digest after the last `@`, the first path component a registry when it
/// has a `.` or `:`, and a tag dropped from the name.
fn parse_image(image: &str) -> Option<ImageParts> {
    if let Ok(reference) = biffix_workflow::images::ImageRef::parse(image) {
        return reference.digest.map(|digest| ImageParts {
            registry: Some(reference.registry),
            name: Some(reference.repository),
            digest,
        });
    }
    let (repo, digest) = image.rsplit_once('@')?;
    if !digest.starts_with("sha256:") {
        return None;
    }
    let (registry, name) = match repo.split_once('/') {
        Some((host, name)) if host.contains('.') || host.contains(':') => (Some(host), name),
        _ => (None, repo),
    };
    let name = match name.rsplit_once(':') {
        Some((untagged, tag)) if !tag.contains('/') => untagged,
        _ => name,
    };
    Some(ImageParts {
        registry: registry.map(str::to_string),
        name: (!name.is_empty()).then(|| name.to_string()),
        digest: digest.to_string(),
    })
}

fn image_entity(id: &str, digest: &str, registry: Option<&str>, name: Option<&str>) -> Value {
    let mut image = json!({
        "@id": id,
        "@type": "ContainerImage",
        "additionalType": {"@id": "https://w3id.org/ro/terms/workflow-run#DockerImage"},
        "sha256": digest.strip_prefix("sha256:").unwrap_or(digest),
    });
    if let Some(registry) = registry {
        image["registry"] = json!(registry);
    }
    if let Some(name) = name {
        image["name"] = json!(name);
    }
    image
}

/// The run's rules/processes in a dependency order (a producer before its consumers), from the
/// run's DAG, then any group seen only on tasks.
///
/// Deliberately not `biffix_core::sample_progress`'s step order: this one breaks ties first-in-
/// first-out over the DAG's node order and seeds task-only groups as roots, and its order is what
/// published provenance documents carry; the sample grid orders ready steps by name and puts
/// DAG-less groups last.
fn step_order(detail: &RunDetailView, engine: Option<Engine>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut nodes: Vec<String> = Vec::new();
    let groups = detail.tasks.iter().map(|t| task_group(t, engine));
    for g in detail.dag_nodes.iter().cloned().chain(groups) {
        if seen.insert(g.clone()) {
            nodes.push(g);
        }
    }
    let mut indegree: BTreeMap<&str, usize> = nodes.iter().map(|n| (n.as_str(), 0)).collect();
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in &detail.dag_edges {
        if indegree.contains_key(from.as_str()) && indegree.contains_key(to.as_str()) {
            next.entry(from.as_str()).or_default().push(to.as_str());
            *indegree.get_mut(to.as_str()).expect("present") += 1;
        }
    }
    let mut ready: VecDeque<&str> =
        nodes.iter().map(String::as_str).filter(|n| indegree[n] == 0).collect();
    let mut order: Vec<String> = Vec::new();
    while let Some(n) = ready.pop_front() {
        order.push(n.to_string());
        for m in next.get(n).into_iter().flatten() {
            let d = indegree.get_mut(m).expect("present");
            *d -= 1;
            if *d == 0 {
                ready.push_back(m);
            }
        }
    }
    // A cycle (never expected) leaves nodes behind (never brought to indegree 0); keep them, in
    // their original order.
    order.extend(nodes.iter().filter(|n| indegree[n.as_str()] > 0).cloned());
    order
}

/// The rule/process a task belongs to: its recorded group, else from its key — a Snakemake key's
/// rule (before the first `:`), a Nextflow key's full process path (before its ` (tag)`).
pub(crate) fn task_group(task: &Task, engine: Option<Engine>) -> String {
    if let Some(group) = &task.group {
        return group.clone();
    }
    let key = task.task_key.as_str();
    match engine {
        Some(Engine::Nextflow) => key.split_once(" (").map_or(key, |(p, _)| p).trim(),
        _ => key.split(':').next().unwrap_or(key),
    }
    .to_string()
}

/// Why a failed task failed, from its winning attempt.
fn task_error(task: &Task) -> Option<String> {
    if task.state != "failed" {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(code) = task.exit_code {
        parts.push(format!("exit code {code}"));
    }
    if let Some(cat) = &task.termination_category {
        parts.push(cat.replace('_', " "));
    }
    Some(if parts.is_empty() { "failed".into() } else { parts.join("; ") })
}

/// A task's resource usage: attempts, instance type, cost, and whether it was a cache hit.
fn task_usage(key: &str, task: &Task) -> Vec<Value> {
    let mut usage = Vec::new();
    let task_slug = slug(key);
    let mut push = |name: &str, value: Value, unit: Option<&str>| {
        let mut pv = json!({
            "@id": format!("#usage/{task_slug}/{name}"),
            "@type": "PropertyValue",
            "name": name,
            "propertyID": format!("{TERMS}{}", name.to_lowercase()),
            "value": value,
        });
        if let Some(unit) = unit {
            pv["unitCode"] = json!(unit);
        }
        usage.push(pv);
    };
    if task.attempts > 0 {
        push("attempts", json!(task.attempts.to_string()), None);
    }
    if let Some(itype) = task.host.as_ref().and_then(|h| h.instance_type.as_deref()) {
        push("instanceType", json!(itype), None);
    }
    if let Some(usd) = task.cost_usd {
        push(
            "estimatedCost",
            json!(format!("{usd:.6}")),
            Some("https://qudt.org/vocab/currency/USD"),
        );
    }
    if task.cached {
        push("cached", json!("true"), None);
    }
    usage
}

fn action_status(state: &str) -> &'static str {
    match state {
        "succeeded" => "http://schema.org/CompletedActionStatus",
        "failed" | "cancelled" => "http://schema.org/FailedActionStatus",
        "running" => "http://schema.org/ActiveActionStatus",
        _ => "http://schema.org/PotentialActionStatus",
    }
}

/// The WorkflowHub `ComputerLanguage` entity for an engine.
fn language_entity(engine: Engine) -> Value {
    match engine {
        Engine::Snakemake => json!({
            "@id": "https://w3id.org/workflowhub/workflow-ro-crate#snakemake",
            "@type": "ComputerLanguage",
            "name": "Snakemake",
            "identifier": {"@id": "https://snakemake.readthedocs.io"},
            "url": {"@id": "https://snakemake.readthedocs.io"},
        }),
        Engine::Nextflow => json!({
            "@id": "https://w3id.org/workflowhub/workflow-ro-crate#nextflow",
            "@type": "ComputerLanguage",
            "name": "Nextflow",
            "identifier": {"@id": "https://www.nextflow.io/"},
            "url": {"@id": "https://www.nextflow.io/"},
        }),
    }
}

fn profile_name(profile: &str) -> &'static str {
    if profile.contains("/process/") {
        "Process Run Crate"
    } else if profile.contains("/workflow/") {
        "Workflow Run Crate"
    } else if profile.contains("/provenance/") {
        "Provenance Run Crate"
    } else {
        "Workflow RO-Crate"
    }
}

/// The `s3://` URIs a param's value names: a URI, or a non-empty list of only URIs.
fn file_values(value: &Value) -> Option<Vec<&str>> {
    let is_uri = |s: &&str| s.starts_with("s3://");
    match value {
        Value::String(s) => Some(vec![s.as_str()]).filter(|v| v.iter().all(is_uri)),
        Value::Array(items) if !items.is_empty() => {
            items.iter().map(|i| i.as_str().filter(is_uri)).collect::<Option<Vec<_>>>()
        }
        _ => None,
    }
}

/// A `PropertyValue` param's `additionalType`: the `DataType` of its value.
fn data_type(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "Boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "Integer",
        Value::Number(_) => "Float",
        _ => "Text",
    }
}

/// A `PropertyValue`'s `value`: strings as-is, anything else as compact JSON.
fn property_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn insert_time(m: &mut Map<String, Value>, key: &str, secs: Option<f64>) {
    if let Some(secs) = secs {
        m.insert(key.into(), json!(iso8601((secs * 1e6) as i64)));
    }
}

/// `YYYY-MM-DDTHH:MM:SS+00:00` for epoch microseconds (a numeric offset rather than `Z`, the form
/// the WRROC validator's pattern accepts).
pub(crate) fn iso8601(micros: i64) -> String {
    format!("{}+00:00", biffix_core::time::utc_datetime(micros, 'T'))
}

/// A fragment-safe form of a name: every byte but `[A-Za-z0-9._~-]` is percent-encoded (`/` and
/// `#` included), so two distinct names never share a slug, and a slug never contains the `/`
/// that separates an `@id`'s namespace from it.
fn slug(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// Drop repeated `{"@id": …}` references, keeping the first.
fn dedup_ids(refs: &mut Vec<Value>) {
    let mut seen = BTreeSet::new();
    refs.retain(|r| seen.insert(r["@id"].as_str().unwrap_or_default().to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_never_contain_a_namespace_separator() {
        assert_eq!(slug("a:b=1/c#d"), "a%3Ab%3D1%2Fc%23d");
        assert_ne!(slug("a:b=1"), slug("a:b_1"));
    }

    #[test]
    fn a_pinned_image_reference_splits_into_registry_name_and_digest() {
        let split = |image: &str| parse_image(image).map(|p| (p.registry, p.name, p.digest));
        let some = |s: &str| Some(s.to_string());
        let digest = format!("sha256:{}", "ab".repeat(32));
        // As the image map's parser splits it: a tag is not part of the name, `localhost` is a
        // registry, Docker Hub is named, the digest lowercased.
        assert_eq!(
            split(&format!("123.dkr.ecr.cn-north-1.amazonaws.com.cn/team/wf@{digest}")),
            Some((
                some("123.dkr.ecr.cn-north-1.amazonaws.com.cn"),
                some("team/wf"),
                digest.clone()
            ))
        );
        assert_eq!(
            split(&format!("reg.example.com/a:v1@{digest}")),
            Some((some("reg.example.com"), some("a"), digest.clone()))
        );
        assert_eq!(
            split(&format!("localhost/foo@{digest}")),
            Some((some("localhost"), some("foo"), digest.clone()))
        );
        assert_eq!(
            split(&format!("ubuntu@{}", digest.to_uppercase().replace("SHA256", "sha256"))),
            Some((some("docker.io"), some("library/ubuntu"), digest.clone()))
        );
        // One the parser refuses (a short digest, as an engine may report it): split leniently,
        // its tag still dropped.
        assert_eq!(
            split("reg.example.com/team/wf:v2@sha256:ab"),
            Some((some("reg.example.com"), some("team/wf"), "sha256:ab".to_string()))
        );
        // Not pinned: none.
        assert!(parse_image("ubuntu:24.04").is_none());
    }
}
