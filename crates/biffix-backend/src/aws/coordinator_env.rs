//! The coordinator's environment: the contract `docker/coordinator/entrypoint.sh` documents, as
//! Batch `SubmitJob` sends it ([`coordinator_env`]), with the coordinator job's tags and the size
//! Batch allows its container overrides.
//!
//! Every variable's name is spelled once, below. The tests hold the backend to the entrypoint's
//! header, which documents what a launch sends (`FIXED`, `OPTIONAL`), what the coordinator job
//! definition sets itself (`TEMPLATE`, also checked against
//! `infra/biffix_infra/constructs/coordinator.py`) and the local dry-run switch (`LOCAL`), and
//! nothing more.

use biffix_core::ids::RunId;

use super::batch::CoordinatorLaunch;
use super::AwsError;

/// The run this coordinator drives.
pub(crate) const RUN_ID: &str = "BIFFIX_RUN_ID";
/// The per-run capability token the engine plugin authenticates its events with.
pub(crate) const INGEST_TOKEN: &str = "BIFFIX_INGEST_TOKEN";
/// The ingest route the engine plugin posts its events to.
pub(crate) const INGEST_URL: &str = "BIFFIX_INGEST_URL";
/// The workflow source, as JSON.
pub(crate) const WORKFLOW_SOURCE: &str = "BIFFIX_WORKFLOW_SOURCE";
/// The exact commit to check out.
pub(crate) const GIT_SHA: &str = "BIFFIX_GIT_SHA";
/// The workflow engine.
pub(crate) const ENGINE: &str = "BIFFIX_ENGINE";
/// The engine entrypoint, relative to the source root.
pub(crate) const ENTRYPOINT: &str = "BIFFIX_ENTRYPOINT";
/// The run's parameters, as JSON.
pub(crate) const PARAMS: &str = "BIFFIX_PARAMS";
/// The project bucket.
pub(crate) const PROJECT_BUCKET: &str = "BIFFIX_PROJECT_BUCKET";
/// The run's project id, which every child job is tagged with.
pub(crate) const PROJECT: &str = "BIFFIX_PROJECT";
/// The run's key prefix in the project bucket.
pub(crate) const RUN_PREFIX: &str = "BIFFIX_RUN_PREFIX";
/// The job role child jobs run as (the project's run role).
pub(crate) const SNAKEMAKE_JOB_ROLE: &str = "SNAKEMAKE_AWS_BATCH_JOB_ROLE";
/// The tags the Snakemake executor puts on every child job.
pub(crate) const SNAKEMAKE_JOB_TAGS: &str = "SNAKEMAKE_AWS_BATCH_JOB_TAGS";
/// The engine release the version pins.
pub(crate) const ENGINE_VERSION: &str = "BIFFIX_ENGINE_VERSION";
/// The runtime the coordinator variant carries.
pub(crate) const RUNTIME_KEY: &str = "BIFFIX_RUNTIME_KEY";
/// The image child jobs default to.
pub(crate) const WORKER_IMAGE: &str = "BIFFIX_WORKER_IMAGE";
/// The run's image map (`images.json`).
pub(crate) const IMAGE_MAP_URI: &str = "BIFFIX_IMAGE_MAP_URI";
/// The worker queue-class child jobs are submitted to.
pub(crate) const QUEUE_CLASS: &str = "BIFFIX_QUEUE_CLASS";
/// The target to build.
pub(crate) const TARGET: &str = "BIFFIX_TARGET";
/// The engine's default storage prefix.
pub(crate) const OUTPUT_PREFIX: &str = "BIFFIX_OUTPUT_PREFIX";
/// The default task retry count.
pub(crate) const RETRIES: &str = "BIFFIX_RETRIES";
/// Whether to finish the rest of the DAG after a failure.
pub(crate) const KEEP_GOING: &str = "BIFFIX_KEEP_GOING";
/// The Nextflow session the run runs as, or resumes.
pub(crate) const ENGINE_SESSION_ID: &str = "BIFFIX_ENGINE_SESSION_ID";
/// `1`: resume the prior run in the same output prefix.
pub(crate) const RESUME: &str = "BIFFIX_RESUME";
/// The Snakemake between-workflow output cache.
pub(crate) const SNAKEMAKE_CACHE: &str = "BIFFIX_SNAKEMAKE_CACHE";

/// What every launch sends.
#[cfg(test)]
pub(crate) const FIXED: &[&str] = &[
    RUN_ID,
    INGEST_TOKEN,
    INGEST_URL,
    WORKFLOW_SOURCE,
    GIT_SHA,
    ENGINE,
    ENTRYPOINT,
    PARAMS,
    PROJECT_BUCKET,
    PROJECT,
    RUN_PREFIX,
    SNAKEMAKE_JOB_ROLE,
    SNAKEMAKE_JOB_TAGS,
    ENGINE_VERSION,
    RUNTIME_KEY,
];

/// What a launch sends when the run has it.
#[cfg(test)]
pub(crate) const OPTIONAL: &[&str] = &[
    WORKER_IMAGE,
    IMAGE_MAP_URI,
    QUEUE_CLASS,
    TARGET,
    OUTPUT_PREFIX,
    RETRIES,
    KEEP_GOING,
    ENGINE_SESSION_ID,
    RESUME,
    SNAKEMAKE_CACHE,
];

/// What the coordinator job definition (the template every variant clones) sets itself: the
/// worker queue-classes' queues, the region, and the project's task log group.
#[cfg(test)]
pub(crate) const TEMPLATE: &[&str] =
    &["BIFFIX_QUEUE_CLASS_MAP", "BIFFIX_REGION", "BIFFIX_TASK_LOG_GROUP"];

/// What only a local run of the image sets: the dry-run switch.
#[cfg(test)]
pub(crate) const LOCAL: &[&str] = &["BIFFIX_DRY_RUN"];

/// ECS (and so Batch) refuses container overrides longer than this many characters, counting the
/// JSON they are sent as.
const CONTAINER_OVERRIDES_LIMIT: usize = 8192;

/// Room left in [`CONTAINER_OVERRIDES_LIMIT`] for what Batch adds to the overrides we send (the
/// container name and the JSON around it).
const CONTAINER_OVERRIDES_SLACK: usize = 256;

/// The coordinator's environment for `launch`: the contract `docker/coordinator/entrypoint.sh`
/// documents. The source and params go as JSON; the coordinator fetches the source at `git_sha`,
/// materializes the params into the engine config, and runs the engine with the biffix logger and
/// the AWS Batch executor (baked into the variant image). `ingest_url` is the deploy's ingest
/// route, and `worker_image_repo` the tenant workflow repository a version's default image is
/// run from when the version recorded no repository of its own.
pub(crate) fn coordinator_env(
    launch: &CoordinatorLaunch,
    ingest_url: &str,
    worker_image_repo: &str,
) -> Result<Vec<(&'static str, String)>, AwsError> {
    // Never launch on a placeholder: a coordinator given `{}` would run without its source/params.
    fn json(what: &str, value: &impl serde::Serialize) -> Result<String, AwsError> {
        serde_json::to_string(value)
            .map_err(|e| AwsError::Batch(format!("serializing the run's {what}: {e}")))
    }
    let fixed = [
        (RUN_ID, launch.run_id.as_str().to_string()),
        (INGEST_TOKEN, launch.capability_token.clone()),
        (INGEST_URL, ingest_url.to_string()),
        (WORKFLOW_SOURCE, json("source", &launch.source)?),
        (GIT_SHA, launch.git_sha.clone()),
        (ENGINE, launch.engine.as_str().to_string()),
        (ENTRYPOINT, launch.entrypoint.clone()),
        (PARAMS, json("params", &launch.params)?),
        // The run's project: its bucket, its id (the job tag) and its run role, per run (a deploy
        // has several projects; the coordinator image's contract is unchanged).
        (PROJECT_BUCKET, launch.project.bucket.clone()),
        // The engines tag every child job with it (`biffix:project=<it>`): the coordinator role
        // may submit only jobs of its own project.
        (PROJECT, launch.project.id.clone()),
        (RUN_PREFIX, biffix_core::s3_layout::run_prefix(launch.run_id.as_str())),
        // The engine's AWS Batch executor registers child job definitions with this job role, so
        // worker tasks get the project-scoped run role (so a task's blast radius is one project).
        (SNAKEMAKE_JOB_ROLE, launch.project.run_role_arn.clone()),
        // The Snakemake executor tags every worker job with these (the Nextflow path tags them
        // itself, from BIFFIX_RUN_ID): the engine ingest route accepts a task_submitted claim on a
        // job only when its run tag names the claiming run.
        (SNAKEMAKE_JOB_TAGS, format!("{}={}", super::batch::RUN_TAG, launch.run_id.as_str())),
        (ENGINE_VERSION, launch.variant.engine_version.clone()),
        (RUNTIME_KEY, launch.variant.runtime_key.clone()),
    ];
    let optional = [
        // The image tasks default to (`super::batch::worker_image`): the version's default image
        // in its recorded repository (else the tenant workflow repository); for a Snakemake
        // version with no image of its own, the coordinator variant (which carries the same
        // pinned Snakemake: a child job runs `python -m snakemake`); for a Nextflow one, none
        // (each task declares its container).
        (
            WORKER_IMAGE,
            super::batch::worker_image(
                &launch.image_digest,
                launch.worker_image_repo_uri.as_deref(),
                worker_image_repo,
                launch.engine,
                &launch.variant.image,
            ),
        ),
        (IMAGE_MAP_URI, launch.image_map_uri.clone()),
        // The first declared worker queue-class selects the child-job queue.
        (QUEUE_CLASS, launch.queue_classes.first().cloned()),
        (TARGET, launch.target.clone()),
        (OUTPUT_PREFIX, launch.output_prefix.clone()),
        (RETRIES, launch.retries.map(|r| r.to_string())),
        (KEEP_GOING, launch.keep_going.map(|k| k.to_string())),
        (ENGINE_SESSION_ID, launch.engine_session_id.clone()),
        (RESUME, launch.resume.then(|| "1".to_string())),
        (SNAKEMAKE_CACHE, launch.output_cache.clone()),
    ];
    Ok(fixed
        .into_iter()
        .chain(optional.into_iter().filter_map(|(name, value)| Some((name, value?))))
        .collect())
}

/// The coordinator job's tags: its run and project, like every child job's. IAM lets no
/// coordinator re-tag a job that carries a run tag, nor terminate or tag a job of another project
/// (`infra/biffix_infra/stacks/compute_stack.py`), so a job born with them cannot be relabelled as
/// another run's and claimed by it, nor stopped from another project.
pub(crate) fn coordinator_job_tags(
    run_id: &RunId,
    project: &str,
) -> std::collections::HashMap<String, String> {
    [
        (super::batch::RUN_TAG.to_string(), run_id.as_str().to_string()),
        (super::batch::PROJECT_TAG.to_string(), project.to_string()),
    ]
    .into()
}

/// Refuse an environment whose container overrides Batch would reject for their size: the run's
/// parameters (and source) travel in it, so a large parameter set can exceed the limit.
pub(crate) fn check_overrides_size(env: &[(&str, String)]) -> Result<(), AwsError> {
    let environment: Vec<_> = env
        .iter()
        .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
        .collect();
    let size = serde_json::json!({ "environment": environment }).to_string().len();
    let limit = CONTAINER_OVERRIDES_LIMIT - CONTAINER_OVERRIDES_SLACK;
    if size > limit {
        let params = env.iter().find(|(name, _)| *name == PARAMS).map_or(0, |(_, v)| v.len());
        return Err(AwsError::Invalid(format!(
            "the run's settings are too large to launch: AWS Batch allows about {limit} characters \
             of coordinator environment and this run needs {size} ({params} of them parameters); \
             pass large values (long lists of files) as a samplesheet or an s3:// folder instead"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::aws::batch::LaunchVariant;
    use crate::register::workflow::WorkflowSource;
    use biffix_workflow::engines::Engine;

    const DEPLOY_INGEST_URL: &str = "https://api/ingest";
    const DEPLOY_WORKER_REPO: &str = "123.dkr.ecr.us-east-1.amazonaws.com/workers";

    /// A Nextflow launch with nothing optional set.
    fn minimal_launch() -> CoordinatorLaunch {
        CoordinatorLaunch {
            run_id: RunId::new("run-1"),
            project: crate::aws::batch::LaunchProject {
                id: "demo".into(),
                bucket: "proj".into(),
                run_role_arn: "arn:aws:iam::123:role/run".into(),
            },
            coordinator_queue: "coordinator".into(),
            image_digest: String::new(),
            worker_image_repo_uri: None,
            capability_token: "tok".into(),
            source: WorkflowSource::Git { repo: "github.com/o/r".into(), git_ref: "main".into() },
            git_sha: "abc".into(),
            engine: Engine::Nextflow,
            entrypoint: "main.nf".into(),
            target: None,
            output_prefix: None,
            params: serde_json::json!({"n": 1}),
            queue_classes: vec![],
            retries: None,
            keep_going: None,
            engine_session_id: None,
            resume: false,
            output_cache: None,
            variant: LaunchVariant {
                job_definition: "arn:jd".into(),
                image: "coord@sha256:v".into(),
                engine_version: "25.10.4".into(),
                runtime_key: "rk".into(),
            },
            image_map_uri: None,
        }
    }

    fn env_map(launch: &CoordinatorLaunch) -> BTreeMap<&'static str, String> {
        let env = coordinator_env(launch, DEPLOY_INGEST_URL, DEPLOY_WORKER_REPO).unwrap();
        let map: BTreeMap<_, _> = env.iter().cloned().collect();
        assert_eq!(map.len(), env.len(), "a variable is set twice");
        map
    }

    #[test]
    fn a_minimal_launch_sets_only_the_fixed_contract() {
        let launch = minimal_launch();
        let source = serde_json::to_string(&launch.source).unwrap();
        let want: BTreeMap<&str, String> = [
            ("BIFFIX_RUN_ID", "run-1"),
            ("BIFFIX_INGEST_TOKEN", "tok"),
            ("BIFFIX_INGEST_URL", "https://api/ingest"),
            ("BIFFIX_WORKFLOW_SOURCE", source.as_str()),
            ("BIFFIX_GIT_SHA", "abc"),
            ("BIFFIX_ENGINE", "nextflow"),
            ("BIFFIX_ENTRYPOINT", "main.nf"),
            ("BIFFIX_PARAMS", r#"{"n":1}"#),
            ("BIFFIX_PROJECT_BUCKET", "proj"),
            ("BIFFIX_PROJECT", "demo"),
            ("BIFFIX_RUN_PREFIX", "biffix/runs/run-1/"),
            ("SNAKEMAKE_AWS_BATCH_JOB_ROLE", "arn:aws:iam::123:role/run"),
            ("SNAKEMAKE_AWS_BATCH_JOB_TAGS", "biffix:run=run-1"),
            ("BIFFIX_ENGINE_VERSION", "25.10.4"),
            ("BIFFIX_RUNTIME_KEY", "rk"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
        assert_eq!(env_map(&launch), want);
    }

    #[test]
    fn a_full_launch_sets_every_optional_variable() {
        let launch = CoordinatorLaunch {
            image_digest: "sha256:img".into(),
            worker_image_repo_uri: Some("123.dkr.ecr.us-east-1.amazonaws.com/wf".into()),
            target: Some("all".into()),
            output_prefix: Some("s3://proj/out/".into()),
            queue_classes: vec!["gpu".into(), "cpu".into()],
            retries: Some(2),
            keep_going: Some(false),
            engine_session_id: Some("sess".into()),
            resume: true,
            output_cache: Some("s3://proj/biffix/cache/snakemake/".into()),
            image_map_uri: Some("s3://proj/biffix/runs/run-1/inputs/images.json".into()),
            ..minimal_launch()
        };
        let env = env_map(&launch);
        let optional: BTreeMap<&str, &str> = [
            ("BIFFIX_WORKER_IMAGE", "123.dkr.ecr.us-east-1.amazonaws.com/wf@sha256:img"),
            ("BIFFIX_IMAGE_MAP_URI", "s3://proj/biffix/runs/run-1/inputs/images.json"),
            ("BIFFIX_QUEUE_CLASS", "gpu"),
            ("BIFFIX_TARGET", "all"),
            ("BIFFIX_OUTPUT_PREFIX", "s3://proj/out/"),
            ("BIFFIX_RETRIES", "2"),
            ("BIFFIX_KEEP_GOING", "false"),
            ("BIFFIX_ENGINE_SESSION_ID", "sess"),
            ("BIFFIX_RESUME", "1"),
            ("BIFFIX_SNAKEMAKE_CACHE", "s3://proj/biffix/cache/snakemake/"),
        ]
        .into_iter()
        .collect();
        for (name, value) in &optional {
            assert_eq!(env.get(name).map(String::as_str), Some(*value), "{name}");
        }
        assert_eq!(env.len(), env_map(&minimal_launch()).len() + optional.len());
    }

    #[test]
    fn a_snakemake_launch_without_an_image_defaults_tasks_to_its_variant() {
        let launch = CoordinatorLaunch { engine: Engine::Snakemake, ..minimal_launch() };
        assert_eq!(env_map(&launch)["BIFFIX_WORKER_IMAGE"], "coord@sha256:v");
    }

    /// The variable names in `text`'s words: `BIFFIX_*` and `SNAKEMAKE_*`, all capitals.
    fn env_names(text: &str) -> BTreeSet<&str> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| {
                (w.starts_with("BIFFIX_") || w.starts_with("SNAKEMAKE_"))
                    && w.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            })
            .collect()
    }

    fn names(lists: &[&[&'static str]]) -> BTreeSet<&'static str> {
        lists.iter().flat_map(|l| l.iter().copied()).collect()
    }

    /// A launch with every optional variable set.
    fn full_launch() -> CoordinatorLaunch {
        CoordinatorLaunch {
            image_digest: "sha256:img".into(),
            target: Some("all".into()),
            output_prefix: Some("s3://proj/out/".into()),
            queue_classes: vec!["gpu".into()],
            retries: Some(2),
            keep_going: Some(false),
            engine_session_id: Some("sess".into()),
            resume: true,
            output_cache: Some("s3://proj/biffix/cache/snakemake/".into()),
            image_map_uri: Some("s3://proj/images.json".into()),
            ..minimal_launch()
        }
    }

    #[test]
    fn a_launch_sends_the_named_variables() {
        let fixed: BTreeSet<_> = env_map(&minimal_launch()).into_keys().collect();
        assert_eq!(fixed, names(&[FIXED]));
        let sent: BTreeSet<_> = env_map(&full_launch()).into_keys().collect();
        assert_eq!(sent, names(&[FIXED, OPTIONAL]));
        // No name is in two lists.
        let all = [FIXED, OPTIONAL, TEMPLATE, LOCAL];
        assert_eq!(names(&all).len(), all.iter().map(|l| l.len()).sum::<usize>());
    }

    #[test]
    fn the_coordinator_env_is_the_entrypoint_s_documented_contract() {
        // The contract is the comment block at the top of the entrypoint: everything sent, the
        // template's own env and the local-validation switch, and nothing else.
        let script = include_str!("../../../../docker/coordinator/entrypoint.sh");
        let header: String =
            script.lines().take_while(|l| l.starts_with('#')).collect::<Vec<_>>().join("\n");
        assert_eq!(env_names(&header), names(&[FIXED, OPTIONAL, TEMPLATE, LOCAL]));
    }

    #[test]
    fn the_template_env_is_the_coordinator_job_definition_s() {
        // The keys of the coordinator job definition's `environment` (a Python dict literal).
        let construct = include_str!("../../../../infra/biffix_infra/constructs/coordinator.py");
        let keys: BTreeSet<&str> = construct
            .lines()
            .filter_map(|l| l.trim().strip_prefix('"')?.split_once("\":"))
            .map(|(key, _)| key)
            .filter(|key| !env_names(key).is_empty())
            .collect();
        assert_eq!(keys, names(&[TEMPLATE]));
    }

    #[test]
    fn a_coordinator_job_is_tagged_with_its_run_and_project() {
        let tags = coordinator_job_tags(&RunId::new("run-1"), "demo");
        assert_eq!(
            tags,
            [
                ("biffix:run".to_string(), "run-1".to_string()),
                ("biffix:project".to_string(), "demo".to_string()),
            ]
            .into()
        );
    }

    #[test]
    fn the_coordinator_env_names_the_run_s_own_project() {
        let launch = CoordinatorLaunch {
            project: crate::aws::batch::LaunchProject {
                id: "imported".into(),
                bucket: "acme-imported".into(),
                run_role_arn: "arn:aws:iam::123:role/imported-run".into(),
            },
            ..minimal_launch()
        };
        let env = env_map(&launch);
        assert_eq!(env["BIFFIX_PROJECT"], "imported");
        assert_eq!(env["BIFFIX_PROJECT_BUCKET"], "acme-imported");
        assert_eq!(env["SNAKEMAKE_AWS_BATCH_JOB_ROLE"], "arn:aws:iam::123:role/imported-run");
    }

    #[test]
    fn a_launch_too_large_for_batch_is_refused() {
        let env =
            coordinator_env(&minimal_launch(), DEPLOY_INGEST_URL, DEPLOY_WORKER_REPO).unwrap();
        assert!(check_overrides_size(&env).is_ok());
        let files: Vec<String> =
            (0..400).map(|i| format!("s3://proj/data/sample-{i:04}.fastq.gz")).collect();
        let launch =
            CoordinatorLaunch { params: serde_json::json!({ "reads": files }), ..minimal_launch() };
        let env = coordinator_env(&launch, DEPLOY_INGEST_URL, DEPLOY_WORKER_REPO).unwrap();
        match check_overrides_size(&env) {
            Err(AwsError::Invalid(m)) => assert!(m.contains("too large"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}
