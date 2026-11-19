//! The Batch job definitions a run's tasks run on, and the map that hands them to the engines.
//!
//! A run's coordinator runs the workflow's own code, so it must not choose what its tasks' job
//! definitions contain: a definition decides whether a container is privileged and which host
//! paths and devices it sees, on compute environments every project shares. The backend therefore
//! registers them ([`crate::aws::batch::BatchJobs::task_job_definition`]), at launch, one per
//! (project, image tasks run, engine), and IAM denies coordinators every job-definition write
//! (`infra/biffix_infra/constructs/batch_access.py`). The coordinator gets a map of image to
//! definition ([`uri_for`], beside the image map) and the engines submit every task on the
//! definition of its image: the Snakemake executor's `--aws-batch-job-definition-map`, nf-biffix's
//! task handler.
//!
//! Each definition is what the engine itself registered before, less anything a task could use to
//! leave its container: never privileged, no devices, no volumes but the Nextflow tasks' read-only
//! host AWS CLI. The per-task command, environment, vCPUs, memory, GPUs, timeout and spot retries
//! travel at `SubmitJob`, as the engines already send them.
//!
//! Registered at launch rather than at registration: only a launch knows every image the run may
//! name (the version's, the ones given at submit, the coordinator variant a Snakemake version
//! defaults to) and the project's role and log group as they are now. A definition is named by a
//! hash of everything it carries ([`TaskJobDefinition::name`]), so the next launch with the same
//! settings finds and reuses it, and a changed role or log group names a new one.

use std::collections::BTreeMap;

use biffix_workflow::engines::Engine;

/// `/dev/shm` of a Snakemake task's container, in MiB. The executor sized it per job from a
/// rule's `shared_memory_size_mb`, which a pre-registered definition cannot; it now checks that
/// resource against the definition's size, so every definition gets one size large enough for any
/// rule (an in-memory genome index included). It is only a ceiling: what a task writes there counts
/// against the job's own memory, so a large size gives a job no more memory than it asked for.
pub const SNAKEMAKE_SHARED_MEMORY_MIB: i32 = 256 * 1024;

/// Where every Batch host installs the pinned AWS CLI (`HOST_AWS_CLI_PATH`'s directory in
/// `infra/biffix_infra/config/compute.py`), which nf-amazon bind-mounts read-only into each task
/// (`aws.batch.cliPath` in `docker/coordinator/lib/nextflow.sh`) to stage its files.
pub const HOST_AWS_CLI_DIR: &str = "/opt/biffix/aws-cli";

/// The name of the host AWS CLI's volume, as nf-amazon names it.
pub const HOST_AWS_CLI_VOLUME: &str = "aws-cli";

/// The vCPUs and memory (MiB) a task definition is registered with. Placeholders, as the engines
/// register theirs: every task's own come in its `SubmitJob`'s container overrides.
pub const PLACEHOLDER_VCPUS: &str = "1";
pub const PLACEHOLDER_MEMORY_MIB: &str = "1024";

/// The command a task definition is registered with: a placeholder, as nf-amazon's (`true`); each
/// task's command comes in its container overrides.
pub const PLACEHOLDER_COMMAND: &str = "true";

/// Bumped when what a definition carries changes in a way [`TaskJobDefinition`]'s fields do not
/// show (a new setting), so the next launch registers definitions with it rather than reuse ones
/// without it.
const FORMAT: u32 = 1;

/// One task job definition: what tasks of `image` in `project` run on, for `engine`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskJobDefinition {
    pub engine: Engine,
    /// The project id: the definition is tagged with it ([`crate::aws::batch::PROJECT_TAG`]), and
    /// IAM lets only that project's coordinator submit it.
    pub project: String,
    /// The tenant image tasks run (a value of the run's image map).
    pub image: String,
    /// The project's run role, which its tasks run as.
    pub job_role_arn: String,
    /// The project's task log group.
    pub log_group: String,
}

impl TaskJobDefinition {
    /// `/dev/shm`'s size in MiB, when the definition sets one (Snakemake's tasks).
    pub fn shared_memory_mib(&self) -> Option<i32> {
        (self.engine == Engine::Snakemake).then_some(SNAKEMAKE_SHARED_MEMORY_MIB)
    }

    /// Whether the definition mounts the host AWS CLI read-only (Nextflow's tasks).
    pub fn mounts_host_aws_cli(&self) -> bool {
        self.engine == Engine::Nextflow
    }

    /// Its Batch name: `<prefix><sm|nf>-<32 hex>`, the hex a hash of everything it carries, so
    /// two definitions share a name only if they are the same (at most 128 characters for any
    /// prefix the deploy makes, `biffix-<tenant>_task-` with a tenant of at most 32; the `_`, which
    /// no tenant contains, ends the tenant, so no sibling tenant's definitions share the prefix).
    pub fn name(&self, prefix: &str) -> String {
        let engine = match self.engine {
            Engine::Snakemake => "sm",
            Engine::Nextflow => "nf",
        };
        let canonical = [
            format!("v{FORMAT}"),
            self.engine.as_str().to_string(),
            self.project.clone(),
            self.image.clone(),
            self.job_role_arn.clone(),
            self.log_group.clone(),
            self.shared_memory_mib().map_or_else(String::new, |mib| mib.to_string()),
            self.mounts_host_aws_cli().to_string(),
        ]
        .join("\n");
        let hash = crate::sha256_hex(canonical);
        format!("{prefix}{engine}-{}", &hash[..32])
    }
}

/// The task job definitions of a run whose image map is `image_map`, for `engine` in the project
/// `project` (its id, run role and task log group): one per image tasks may run (the map's
/// values), in image order.
pub fn for_image_map(
    image_map: &BTreeMap<String, String>,
    engine: Engine,
    project: &crate::aws::batch::LaunchProject,
) -> Vec<TaskJobDefinition> {
    let images: std::collections::BTreeSet<&String> = image_map.values().collect();
    images
        .into_iter()
        .map(|image| TaskJobDefinition {
            engine,
            project: project.id.clone(),
            image: image.clone(),
            job_role_arn: project.run_role_arn.clone(),
            log_group: project.task_log_group.clone(),
        })
        .collect()
}

/// Where a run's map of image to task job definition is written: beside its image map
/// (`crate::images::map::uri_for`), in the project bucket.
pub fn uri_for(project_bucket: &str, run_id: &str) -> String {
    format!(
        "s3://{project_bucket}/{}inputs/job-definitions.json",
        crate::runs::output_prefix::run_output_prefix(run_id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(engine: Engine) -> TaskJobDefinition {
        TaskJobDefinition {
            engine,
            project: "demo".into(),
            image: "123.dkr.ecr.us-east-1.amazonaws.com/workers@sha256:abc".into(),
            job_role_arn: "arn:aws:iam::123:role/run".into(),
            log_group: "/biffix/t/demo/tasks".into(),
        }
    }

    #[test]
    fn a_definition_is_named_by_everything_it_carries() {
        let sm = definition(Engine::Snakemake);
        let name = sm.name("biffix-t_task-");
        assert!(name.starts_with("biffix-t_task-sm-"), "{name}");
        assert_eq!(name.len(), "biffix-t_task-sm-".len() + 32);
        assert_eq!(name, sm.clone().name("biffix-t_task-"));
        // The longest prefix a deploy makes still fits Batch's 128 characters.
        let tenant = "t".repeat(32);
        assert!(sm.name(&format!("biffix-{tenant}_task-")).len() <= 128);
        assert!(definition(Engine::Nextflow).name("p-").starts_with("p-nf-"));
        // Any change names another definition.
        let changes: [fn(&mut TaskJobDefinition); 5] = [
            |d| d.engine = Engine::Nextflow,
            |d| d.project = "other".into(),
            |d| d.image.push('d'),
            |d| d.job_role_arn.push('2'),
            |d| d.log_group.push('2'),
        ];
        for change in changes {
            let mut other = sm.clone();
            change(&mut other);
            assert_ne!(other.name("biffix-t_task-"), name);
        }
    }

    #[test]
    fn only_snakemake_sets_shared_memory_and_only_nextflow_mounts_the_host_cli() {
        assert_eq!(
            definition(Engine::Snakemake).shared_memory_mib(),
            Some(SNAKEMAKE_SHARED_MEMORY_MIB)
        );
        assert!(!definition(Engine::Snakemake).mounts_host_aws_cli());
        assert_eq!(definition(Engine::Nextflow).shared_memory_mib(), None);
        assert!(definition(Engine::Nextflow).mounts_host_aws_cli());
    }

    #[test]
    fn a_run_gets_one_definition_per_image_its_tasks_may_run() {
        let worker = "repo@sha256:w".to_string();
        let map = BTreeMap::from([
            ("ubuntu:24.04".to_string(), worker.clone()),
            ("docker.io/library/ubuntu:24.04".to_string(), worker.clone()),
            ("$default".to_string(), worker.clone()),
            (worker.clone(), worker.clone()),
            ("quay.io/b/fastqc:1".to_string(), "repo@sha256:f".to_string()),
        ]);
        let project = crate::aws::batch::LaunchProject {
            id: "demo".into(),
            bucket: "b".into(),
            run_role_arn: "arn:role".into(),
            task_log_group: "/g".into(),
        };
        let definitions = for_image_map(&map, Engine::Nextflow, &project);
        assert_eq!(
            definitions.iter().map(|d| d.image.as_str()).collect::<Vec<_>>(),
            ["repo@sha256:f", "repo@sha256:w"]
        );
        assert!(definitions.iter().all(|d| d.project == "demo"
            && d.job_role_arn == "arn:role"
            && d.log_group == "/g"
            && d.engine == Engine::Nextflow));
    }

    /// The host AWS CLI the Nextflow definitions mount is where every host installs it
    /// (`infra/biffix_infra/config/compute.py`) and where the coordinator tells nf-amazon to find it
    /// (`docker/coordinator/lib/nextflow.sh`).
    #[test]
    fn the_host_aws_cli_mount_is_where_hosts_install_it_and_nextflow_runs_it() {
        let cli = format!("{HOST_AWS_CLI_DIR}/bin/aws");
        let nextflow = include_str!("../../../../docker/coordinator/lib/nextflow.sh");
        assert!(nextflow.contains(&format!("aws.batch.cliPath = '{cli}'")), "{cli}");
        let compute = include_str!("../../../../infra/biffix_infra/config/compute.py");
        assert!(compute.contains(&format!("HOST_AWS_CLI_PATH = \"{cli}\"")), "{cli}");
    }

    #[test]
    fn the_definition_map_sits_beside_the_image_map() {
        let images = crate::images::map::uri_for("b", "run-1");
        let definitions = uri_for("b", "run-1");
        assert_eq!(definitions.rsplit_once('/').unwrap().0, images.rsplit_once('/').unwrap().0);
        assert!(definitions.ends_with("/inputs/job-definitions.json"));
    }
}
