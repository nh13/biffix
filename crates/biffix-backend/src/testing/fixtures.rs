//! Fixture builders: projects, the two-project deploy, a registered version, a run and a task.

use std::sync::Arc;

use crate::config::projects::{ProjectContext, Projects, ProjectsHandle};
use crate::store::RunDetailView;
use biffix_api::RunSummary;

use super::runtime_fields;

/// A project context named `id` with bucket `bucket` and nothing else configured: its run role and
/// coordinator template are empty (the fake's canned role and deployed template).
pub fn project(id: &str, bucket: &str) -> ProjectContext {
    ProjectContext { id: id.into(), bucket: bucket.into(), ..Default::default() }
}

/// A deploy's projects that never change: what [`super::TestState::with_projects`] takes.
pub fn fixed_projects(projects: Projects) -> Arc<ProjectsHandle> {
    Arc::new(ProjectsHandle::fixed(projects))
}

/// The projects of a single-project deploy: `ctx` alone.
pub fn one_project(ctx: ProjectContext) -> Arc<ProjectsHandle> {
    fixed_projects(Projects::single(ctx))
}

/// The projects of a single-project deploy whose one project (`demo`) has bucket `bucket`: what
/// most tests that need a project bucket use.
pub fn bucket_project(bucket: &str) -> Arc<ProjectsHandle> {
    one_project(project("demo", bucket))
}

/// Project A of [`two_projects`]: the default.
pub const PROJECT_A: &str = "genomics";
pub const BUCKET_A: &str = "acme-genomics";
/// Project B of [`two_projects`].
pub const PROJECT_B: &str = "imported";
pub const BUCKET_B: &str = "acme-imported";
/// Project B's own coordinator template, run role and task log group.
pub const TEMPLATE_B: &str =
    "arn:aws:batch:us-east-1:123:job-definition/biffix-acme-imported-coordinator:4";
pub const RUN_ROLE_B: &str = "arn:aws:iam::123:role/imported-run";
pub const LOG_GROUP_B: &str = "/biffix/project-imported/batch";

/// A deploy of two projects: A (`genomics`, the default, uploads and viewer CORS on, its
/// template the fake's deployed one) and B (`imported`, its own run role, template and
/// task log group, uploads off, one data source of its own).
pub fn two_projects() -> Projects {
    let a = ProjectContext {
        run_role_arn: "arn:aws:iam::123:role/genomics-run".into(),
        task_log_group: "/biffix/project-genomics/batch".into(),
        uploads: true,
        viewer_cors: true,
        ..project(PROJECT_A, BUCKET_A)
    };
    let b = ProjectContext {
        run_role_arn: RUN_ROLE_B.into(),
        coordinator_job_definition: TEMPLATE_B.into(),
        task_log_group: LOG_GROUP_B.into(),
        uploads: false,
        viewer_cors: false,
        data_sources: crate::config::data_sources::parse_sources(
            r#"[{"id":"core","name":"Core","bucket":"acme-core","prefix":"deliveries/"}]"#,
        )
        .unwrap(),
        ..project(PROJECT_B, BUCKET_B)
    };
    Projects::new(vec![a, b], PROJECT_A).unwrap()
}

/// Version `abc:hash` of workflow `project`/`name` (staged under `s3://proj/workflow-sources/v1/`)
/// on `engine`, declaring `reports` (its form schema's `reports`, as JSON): what a reports test
/// registers so a run of `abc:hash` resolves them.
pub fn version_with_reports(
    name: &str,
    project: &str,
    engine: biffix_workflow::engines::Engine,
    reports: serde_json::Value,
) -> crate::store::registry::RegisteredVersion {
    use crate::register::workflow::{WorkflowSource, WorkflowVersion};
    use biffix_workflow::engines::Engine;
    let (engine_version, entrypoint) = match engine {
        Engine::Nextflow => ("25.10.4", "main.nf"),
        _ => ("9.27.0", "Snakefile"),
    };
    let (runtime_key, runtime_recipe) = runtime_fields(engine.as_str(), engine_version);
    crate::store::registry::RegisteredVersion {
        name: name.into(),
        project: project.into(),
        source: WorkflowSource::S3 { uri: "s3://proj/workflow-sources/v1/".into(), sha256: None },
        version: WorkflowVersion {
            id: "abc:hash".into(),
            git_sha: "abc".into(),
            source_content_hash: "hash".into(),
            image_digest: "sha256:1a2b".into(),
            image_repository: None,
            image_registry_id: None,
            image_region: None,
            engine,
            engine_version: engine_version.into(),
            entrypoint: entrypoint.into(),
            target: None,
            form_schema: serde_json::json!({"fields": [], "warnings": [], "reports": reports}),
            queue_classes: vec![],
            about_markdown: None,
            runtime_key,
            runtime_recipe,
            build: 0,
        },
        created_by: None,
        created_by_label: None,
        created_at_micros: 0,
        registration_inputs: Default::default(),
        registration_inputs_hash: String::new(),
        images: Vec::new(),
    }
}

/// A minimal projected run in `state` with all metrics/metadata empty — the shape several test
/// crates seed when they only assert on `run_id`/`state`. Task counts are still derived by the
/// store's read path (`with_task_counts`), so nothing observable is lost versus a hand-built copy.
pub fn run_detail(run_id: &str, state: &str) -> RunDetailView {
    RunDetailView {
        run: RunSummary {
            run_id: run_id.to_string().into(),
            state: state.into(),
            terminal: false,
            completed: None,
            total: None,
            submitted_at: None,
            completed_at: None,
            submitter: None,
            submitter_label: None,
            run_name: None,
            workflow: None,
            workflow_name: None,
            workflow_title: None,
            engine: None,
            project: None,
            tasks_observed: 0,
            tasks_succeeded: 0,
            tasks_running: 0,
            tasks_queued: 0,
            tasks_failed: 0,
            tasks_cached: 0,
            outputs_total: 0,
            outputs_reused: 0,
            resumed: false,
            input_uris: vec![],
            samples: None,
            experiment_id: None,
            cost_usd: None,
            cost_pending: false,
        },
        tasks: vec![],
        params: serde_json::Value::Null,
        coordinator_job_id: None,
        coordinator_log_stream: None,
        coordinator_stop: Default::default(),
        coordinator_host: None,
        dag_nodes: vec![],
        dag_edges: vec![],
        output_prefix: None,
        engine_session_id: None,
        session_mismatch: false,
        resumed_from: None,
        input_tables: None,
        cost: None,
        task_groups: vec![],
        reference_bundle: None,
        retention: None,
        work_deleted_at_micros: None,
        launched_at_micros: None,
        cancelled_by: None,
        workflow_registered_at_micros: None,
    }
}

/// `detail` launched at `at_micros` (`None`: never launched), as the projection records it.
pub fn launched_at(mut detail: RunDetailView, at_micros: Option<i64>) -> RunDetailView {
    detail.launched_at_micros = at_micros;
    detail
}

/// A task row in `state` with one attempt and nothing else recorded but its stream and job — the
/// shape the read, cancel and provenance tests start from (a test sets any other field with
/// struct-update syntax).
pub fn task_view(
    key: &str,
    state: &str,
    log_stream: Option<&str>,
    batch_job_id: Option<&str>,
) -> biffix_api::Task {
    biffix_api::Task {
        task_key: biffix_core::ids::TaskKey::new(key),
        state: state.into(),
        attempts: 1,
        exit_code: None,
        batch_job_id: batch_job_id.map(Into::into),
        log_stream: log_stream.map(String::from),
        termination_category: None,
        instance_id: None,
        started_at: None,
        finished_at: None,
        cached: false,
        group: None,
        deps: vec![],
        feeds: vec![],
        cost_usd: None,
        host: None,
        spot_warned_at: None,
        image: None,
        verdict_reason: None,
        requested_vcpus: None,
        requested_memory_mib: None,
    }
}

/// An output `uri` run `run` registered (its id as the fold derives it), from `task` (its first
/// attempt) when given, with `role`; no size, ETag or cached HEAD.
pub fn output_row(
    run: &str,
    uri: &str,
    task: Option<&str>,
    role: &str,
) -> crate::store::outputs::OutputRow {
    crate::store::outputs::OutputRow {
        id: biffix_core::reducer::output_id(run, uri),
        s3_uri: uri.into(),
        path: None,
        task_key: task.map(str::to_string),
        attempt: task.map(|_| 1),
        role: role.into(),
        reused: false,
        size: None,
        etag: None,
        meta: None,
        run_id: biffix_core::ids::RunId::new(run),
    }
}
