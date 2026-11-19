//! Single-job runs: a run that executes as one AWS Batch job on one whole host (see
//! `biffix_workflow::single_job`). At submit, [`resolve`] turns the run's request, laid over its
//! version's default, into a [`SingleJobUse`]: the deploy's single-job queue-class, the exact
//! instance type its size is taken from, and the whole-host vCPUs and memory its job is given. The
//! launch plan keeps it, so a parked run launches at exactly that size, and the run's anchor
//! records it.
//!
//! One run per host is what keeps two projects' runs apart (their jobs share no host): the job asks
//! for the host's whole allocatable share, and each single-job queue-class runs instance types of
//! one size only, so no host of it can take two such jobs. That one-size rule is the deploy
//! config's (`check_single_job` in `infra/biffix_infra/config/compute.py`), its single source of
//! truth; the backend does not check it again.

use std::collections::BTreeMap;

use biffix_core::event::SingleJobUse;
use biffix_workflow::engines::Engine;
use biffix_workflow::single_job::{SingleJobSize, SingleJobSpec};

use crate::aws::ec2::{InstanceTypeInfo, Resources};
use crate::config::single_job::SingleJobQueue;
use crate::error::ServiceError;
use crate::AppState;

/// The share of a host's nominal memory left out of a single-job run's request, in percent, plus
/// [`MEMORY_RESERVE_MIB`]: the kernel and the ECS agent keep some, and a host registers with ECS
/// less memory than its type's nominal size. A job asking for more than the host registered would
/// never be placed, so the request stays under it.
pub(crate) const MEMORY_RESERVE_PERCENT: u64 = 6;

/// The fixed part of the memory left out of a single-job run's request, MiB.
pub(crate) const MEMORY_RESERVE_MIB: u64 = 256;

/// What a single-job run's job is given on a host of `size`: every vCPU, and its memory less the
/// reserve, rounded down to 256 MiB.
pub(crate) fn whole_host(size: Resources) -> Resources {
    let reserve = size.memory_mib * MEMORY_RESERVE_PERCENT / 100 + MEMORY_RESERVE_MIB;
    let memory_mib = size.memory_mib.saturating_sub(reserve) / 256 * 256;
    Resources { vcpus: size.vcpus, memory_mib }
}

/// The memory a single-job run's tasks get, MiB, when its job is given `job_memory_mib`: the job's
/// memory less what `engine` keeps for itself. Nextflow keeps its JVM heap (1/16 of the job, 1 to
/// 8 GiB) and slack for the JVM's own overhead and the image mounts (1/32, at least 1 GiB);
/// Snakemake keeps 1/16, 2 to 8 GiB. Zero when the engine's reserve is all of it (the coordinator
/// then refuses to start).
///
/// A mirror of the coordinator's `single_job_memory` (`docker/coordinator/lib/single_job.sh`),
/// which is what the engine is actually told: change the two together. Both are pinned to the same
/// values by their tests.
pub(crate) fn task_memory_mib(engine: Engine, job_memory_mib: u64) -> u64 {
    let reserve = match engine {
        Engine::Nextflow => {
            let heap = (job_memory_mib / 16).clamp(1024, 8192);
            let slack = (job_memory_mib / 32).max(1024);
            heap + slack
        }
        Engine::Snakemake => (job_memory_mib / 16).clamp(2048, 8192),
    };
    job_memory_mib.saturating_sub(reserve)
}

/// What a single-job run's job is given, as submit resolved it.
pub(crate) fn resources(single: &SingleJobUse) -> Resources {
    Resources { vcpus: single.vcpus, memory_mib: single.memory_mib }
}

/// The deploy's job queue (ARN) for `single`'s queue-class; an error naming why there is none
/// when the deploy has the queue-class no longer, or no longer runs `single`'s instance type on it
/// (a parked run's queue-class changed by a deploy: its whole-host size would no longer fit, or no
/// longer fill, the queue-class's hosts).
pub(crate) fn queue<'a>(
    single: &SingleJobUse,
    queues: &'a [SingleJobQueue],
) -> Result<&'a str, String> {
    let queue = queues.iter().find(|q| q.name == single.queue_class).ok_or_else(|| {
        format!(
            "the single-job queue-class `{}` is no longer configured on this deploy",
            single.queue_class
        )
    })?;
    if !queue.instance_types.contains(&single.instance_type) {
        return Err(format!(
            "instance type `{}` is no longer in single-job queue-class `{}` ({}); submit the run \
             again",
            single.instance_type,
            single.queue_class,
            queue.instance_types.join(", ")
        ));
    }
    Ok(&queue.queue)
}

/// Resolve a submit's single-job `request` for an `engine` version whose form allows it with
/// `allowed` (its `single_job:` default; `None`: the version does not allow it). Describes the
/// queue-class's instance types (EC2), then [`choose`]s. A request the version or the queue-class
/// cannot serve is a 400 naming why; on a deploy with no single-job queue-class, a 409 (as for a
/// project with no single-job template: what the deploy offers, not the request, is missing).
pub(crate) async fn resolve(
    state: &AppState,
    allowed: Option<&SingleJobSpec>,
    request: &SingleJobSpec,
    engine: Engine,
) -> Result<SingleJobUse, ServiceError> {
    let Some(version) = allowed else {
        return Err(ServiceError::Invalid(
            "this workflow version does not allow single-job runs (its biffix.yaml has no \
             `single_job:`); run it without single_job, or register a version that allows it"
                .into(),
        ));
    };
    let queues = &state.deploy.single_job_queues;
    // A version that names no queue-class sized itself for the deploy's default (the first), so
    // a run naming that one by name keeps the version's size, as one naming none does.
    let default = queue_for(queues, None)?;
    let version = SingleJobSpec {
        queue_class: version.queue_class.clone().or_else(|| Some(default.name.clone())),
        ..version.clone()
    };
    let spec = request.over(&version);
    let queue = queue_for(queues, spec.queue_class.as_deref())?;
    let described = state.cloud.describe_instance_types(&queue.instance_types).await?;
    choose(&spec, queue, &described, engine).map_err(ServiceError::Invalid)
}

/// The deploy's single-job queue-class named `name`, else its first: a 409 when the deploy has
/// none at all, a 400 naming the ones it has when it has none of that name.
fn queue_for<'a>(
    queues: &'a [SingleJobQueue],
    name: Option<&str>,
) -> Result<&'a SingleJobQueue, ServiceError> {
    let Some(first) = queues.first() else {
        return Err(ServiceError::Conflict(
            "this deploy has no single-job queue-class (a queue-class with `single_job: true` in \
             its deploy config)"
                .into(),
        ));
    };
    let Some(name) = name else {
        return Ok(first);
    };
    queues.iter().find(|q| q.name == name).ok_or_else(|| {
        let names = queues.iter().map(|q| format!("`{}`", q.name)).collect::<Vec<_>>().join(", ");
        ServiceError::Invalid(format!(
            "no single-job queue-class `{name}` on this deploy (it has: {names})"
        ))
    })
}

/// The launch for `spec` on `queue`, whose instance types EC2 `described`, for an `engine` run:
/// the type `spec` names, else the smallest whose tasks get at least its vCPUs and memory, else
/// (no size given) the smallest of the queue-class; its whole host ([`whole_host`]). A
/// `memory_gib` is compared with the memory the run's tasks get there ([`task_memory_mib`] of the
/// whole host), not the type's nominal memory. Refused when a type of the queue-class is not plain
/// x86_64 (Batch may place the job on any of them): one that is not x86_64, or that carries an
/// accelerator. The deploy config refuses such types by name; EC2's description is the
/// authoritative check.
pub(crate) fn choose(
    spec: &SingleJobSpec,
    queue: &SingleJobQueue,
    described: &BTreeMap<String, InstanceTypeInfo>,
    engine: Engine,
) -> Result<SingleJobUse, String> {
    // The queue-class's types EC2 described, smallest first.
    let mut types: Vec<(&str, &InstanceTypeInfo)> = queue
        .instance_types
        .iter()
        .filter_map(|t| described.get(t).map(|info| (t.as_str(), info)))
        .collect();
    types.sort_by_key(|(name, info)| (info.size.vcpus, info.size.memory_mib, *name));
    let listed = || queue.instance_types.join(", ");
    if types.is_empty() {
        return Err(format!(
            "EC2 describes none of single-job queue-class `{}`'s instance types ({})",
            queue.name,
            listed()
        ));
    }
    for (name, info) in &types {
        if !info.architectures.iter().any(|a| a == "x86_64") {
            return Err(format!(
                "instance type `{name}` of single-job queue-class `{}` is not x86_64 (EC2 says: \
                 {}); single-job runs need x86_64 hosts",
                queue.name,
                info.architectures.join(", ")
            ));
        }
        if !info.accelerators.is_empty() {
            return Err(format!(
                "instance type `{name}` of single-job queue-class `{}` carries an accelerator \
                 ({}); single-job hosts can use none",
                queue.name,
                info.accelerators.join(", ")
            ));
        }
    }
    let (instance_type, size) = match spec.size()? {
        Some(SingleJobSize::Instance(want)) => {
            if !queue.instance_types.contains(&want) {
                return Err(format!(
                    "instance type `{want}` is not one of single-job queue-class `{}`'s ({})",
                    queue.name,
                    listed()
                ));
            }
            types.iter().find(|(t, _)| *t == want).map(|(t, info)| (*t, info.size)).ok_or_else(
                || format!("EC2 does not describe instance type `{want}` in this deploy's region"),
            )?
        }
        Some(SingleJobSize::Resources { cpus, memory_gib }) => types
            .iter()
            .map(|(t, info)| (*t, info.size))
            .find(|(_, size)| {
                size.vcpus >= cpus
                    && task_memory_mib(engine, whole_host(*size).memory_mib)
                        >= u64::from(memory_gib) * 1024
            })
            .ok_or_else(|| {
                format!(
                    "no instance type of single-job queue-class `{}` ({}) has {cpus} vCPUs and \
                     leaves {memory_gib} GiB for the run's tasks (after what the host, ECS and \
                     the {engine} engine keep)",
                    queue.name,
                    listed()
                )
            })?,
        None => (types[0].0, types[0].1.size),
    };
    let job = whole_host(size);
    Ok(SingleJobUse {
        queue_class: queue.name.clone(),
        instance_type: instance_type.to_string(),
        vcpus: job.vcpus,
        memory_mib: job.memory_mib,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_class(name: &str, types: &[&str]) -> SingleJobQueue {
        SingleJobQueue {
            name: name.into(),
            queue: format!("arn:aws:batch:r:1:job-queue/{name}"),
            instance_types: types.iter().map(ToString::to_string).collect(),
        }
    }

    fn described() -> BTreeMap<String, InstanceTypeInfo> {
        crate::testing::fake_instance_types()
    }

    fn spec(instance: Option<&str>, cpus: Option<u32>, memory_gib: Option<u32>) -> SingleJobSpec {
        SingleJobSpec {
            queue_class: None,
            instance: instance.map(str::to_string),
            cpus,
            memory_gib,
        }
    }

    fn choose_sm(
        spec: &SingleJobSpec,
        queue: &SingleJobQueue,
        described: &BTreeMap<String, InstanceTypeInfo>,
    ) -> Result<SingleJobUse, String> {
        choose(spec, queue, described, Engine::Snakemake)
    }

    #[test]
    fn the_whole_host_is_every_vcpu_and_the_memory_less_its_reserve() {
        let host = whole_host(Resources { vcpus: 32, memory_mib: 65_536 });
        assert_eq!(host.vcpus, 32);
        // 65536 - (3932 + 256) = 61348, rounded down to 256 MiB.
        assert_eq!(host.memory_mib, 61_184);
        assert_eq!(host.memory_mib % 256, 0);
        // A tiny host never underflows.
        assert_eq!(whole_host(Resources { vcpus: 1, memory_mib: 100 }).memory_mib, 0);
    }

    /// The (engine, job memory MiB) -> task memory MiB pairs the coordinator's
    /// `single_job_memory` is pinned to as well (`docker/coordinator/tests/test_lib.py`).
    #[test]
    fn the_engine_keeps_its_reserve_of_the_jobs_memory_as_the_coordinator_does() {
        for (engine, job, tasks) in [
            (Engine::Nextflow, 8_192, 6_144),
            (Engine::Nextflow, 61_184, 55_448),
            (Engine::Nextflow, 65_536, 59_392),
            (Engine::Nextflow, 524_288, 499_712),
            (Engine::Snakemake, 8_192, 6_144),
            (Engine::Snakemake, 61_184, 57_360),
            (Engine::Snakemake, 65_536, 61_440),
            (Engine::Snakemake, 524_288, 516_096),
        ] {
            assert_eq!(task_memory_mib(engine, job), tasks, "{engine} {job}");
        }
        // A job smaller than the engine's reserve leaves nothing, never underflows.
        assert_eq!(task_memory_mib(Engine::Snakemake, 1_024), 0);
        assert_eq!(task_memory_mib(Engine::Nextflow, 1_024), 0);
    }

    #[test]
    fn an_instance_type_is_used_as_named_and_must_be_the_queue_classes() {
        let q = queue_class("single", &["c6id.8xlarge", "m6id.8xlarge"]);
        let launch = choose_sm(&spec(Some("m6id.8xlarge"), None, None), &q, &described()).unwrap();
        assert_eq!(launch.instance_type, "m6id.8xlarge");
        assert_eq!(launch.queue_class, "single");
        assert_eq!(launch.vcpus, 32);
        let err = choose_sm(&spec(Some("r6id.8xlarge"), None, None), &q, &described()).unwrap_err();
        assert!(err.contains("not one of single-job queue-class `single`"), "{err}");
    }

    #[test]
    fn cpus_and_memory_pick_the_smallest_type_whose_tasks_get_both() {
        let q = queue_class("single", &["r6id.8xlarge", "c6id.8xlarge", "m6id.8xlarge"]);
        let pick = |cpus, gib| choose_sm(&spec(None, Some(cpus), Some(gib)), &q, &described());
        // A c6id.8xlarge (64 GiB) leaves its Snakemake tasks 57360 MiB, 56 GiB: enough for 56,
        // not for 57, nor for the 64 its nominal memory would suggest.
        assert_eq!(pick(32, 56).unwrap().instance_type, "c6id.8xlarge");
        assert_eq!(pick(32, 57).unwrap().instance_type, "m6id.8xlarge");
        assert_eq!(pick(32, 64).unwrap().instance_type, "m6id.8xlarge");
        assert_eq!(pick(16, 100).unwrap().instance_type, "m6id.8xlarge");
        assert_eq!(pick(32, 200).unwrap().instance_type, "r6id.8xlarge");
        let err = pick(64, 64).unwrap_err();
        assert!(err.contains("no instance type"), "{err}");
        // A Nextflow run's engine keeps more: a c6id.8xlarge leaves its tasks 55448 MiB.
        let nf = |gib| {
            choose(&spec(None, Some(32), Some(gib)), &q, &described(), Engine::Nextflow).unwrap()
        };
        assert_eq!(nf(54).instance_type, "c6id.8xlarge");
        assert_eq!(nf(55).instance_type, "m6id.8xlarge");
    }

    #[test]
    fn no_size_is_the_smallest_type() {
        let q = queue_class("single", &["m6id.8xlarge", "c6id.8xlarge"]);
        let launch = choose_sm(&SingleJobSpec::default(), &q, &described()).unwrap();
        assert_eq!(launch.instance_type, "c6id.8xlarge");
    }

    #[test]
    fn types_ec2_does_not_know_are_skipped_and_none_is_an_error() {
        let q = queue_class("single", &["c6id.8xlarge", "z9.huge"]);
        let launch = choose_sm(&SingleJobSpec::default(), &q, &described()).unwrap();
        assert_eq!(launch.instance_type, "c6id.8xlarge");
        let q = queue_class("unknown", &["z9.huge"]);
        let err = choose_sm(&SingleJobSpec::default(), &q, &described()).unwrap_err();
        assert!(err.contains("none of"), "{err}");
    }

    #[test]
    fn a_type_that_is_not_plain_x86_64_is_refused_naming_it() {
        let mut types = described();
        types.get_mut("m6id.8xlarge").unwrap().architectures = vec!["arm64".into()];
        let q = queue_class("single", &["c6id.8xlarge", "m6id.8xlarge"]);
        // Even when the run is sized for another type of the queue-class: Batch may place it on
        // any of them.
        let err = choose_sm(&spec(Some("c6id.8xlarge"), None, None), &q, &types).unwrap_err();
        assert!(err.contains("`m6id.8xlarge`") && err.contains("not x86_64"), "{err}");

        let mut types = described();
        types.get_mut("c6id.8xlarge").unwrap().accelerators = vec!["GPU".into()];
        let err = choose_sm(&SingleJobSpec::default(), &q, &types).unwrap_err();
        assert!(err.contains("`c6id.8xlarge`") && err.contains("accelerator (GPU)"), "{err}");
    }

    #[test]
    fn a_bad_size_is_refused() {
        let q = queue_class("single", &["c6id.8xlarge"]);
        let err = choose_sm(&spec(None, Some(32), None), &q, &described()).unwrap_err();
        assert!(err.contains("cpus and memory_gib must be given together"), "{err}");
    }

    #[test]
    fn the_queue_class_is_the_named_one_else_the_first() {
        let queues = [queue_class("a", &["c6id.8xlarge"]), queue_class("b", &["m6id.8xlarge"])];
        assert_eq!(queue_for(&queues, None).unwrap().name, "a");
        assert_eq!(queue_for(&queues, Some("b")).unwrap().name, "b");
        let err = queue_for(&queues, Some("c")).unwrap_err();
        assert!(
            matches!(&err, ServiceError::Invalid(m) if m.contains("it has: `a`, `b`")),
            "{err}"
        );
        let err = queue_for(&[], None).unwrap_err();
        assert!(
            matches!(&err, ServiceError::Conflict(m) if m.contains("no single-job queue-class")),
            "{err}"
        );
    }

    #[test]
    fn a_type_the_queue_class_lists_but_ec2_does_not_describe_says_so() {
        let q = queue_class("single", &["c6id.8xlarge", "z9.huge"]);
        let err = choose_sm(&spec(Some("z9.huge"), None, None), &q, &described()).unwrap_err();
        assert!(err.contains("EC2 does not describe instance type `z9.huge`"), "{err}");
    }

    #[test]
    fn a_launch_names_its_queue_while_the_deploy_has_it() {
        let single = SingleJobUse {
            queue_class: "b".into(),
            instance_type: "m6id.8xlarge".into(),
            vcpus: 32,
            memory_mib: 1024,
        };
        assert_eq!(resources(&single), Resources { vcpus: 32, memory_mib: 1024 });
        let queues = [queue_class("a", &["c6id.8xlarge"]), queue_class("b", &["m6id.8xlarge"])];
        assert_eq!(queue(&single, &queues).unwrap(), "arn:aws:batch:r:1:job-queue/b");
        assert!(queue(&single, &queues[..1]).unwrap_err().contains("no longer configured"));
        // Its type gone from the queue-class: its size no longer fits the hosts.
        let changed = [queue_class("b", &["m6id.16xlarge"])];
        let err = queue(&single, &changed).unwrap_err();
        assert!(err.contains("`m6id.8xlarge` is no longer in single-job queue-class `b`"), "{err}");
    }
}
