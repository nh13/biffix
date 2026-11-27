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

mod images;
pub(crate) use images::check_images;

use std::collections::{BTreeMap, BTreeSet};

use biffix_core::event::SingleJobUse;
use biffix_workflow::engines::Engine;
use biffix_workflow::images::Arch;
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

/// The memory a single-job run's Snakemake keeps for each job that may run at once (one per vCPU),
/// MiB: each job runs a Snakemake of its own (the biffix executor), which no job's `mem_mb` counts.
pub(crate) const SNAKEMAKE_JOB_RESERVE_MIB: u64 = 160;

/// The memory a single-job run's tasks get, MiB, when its job is given `job`: the job's memory
/// less what `engine` keeps for itself. Nextflow keeps its JVM heap (1/16 of the job, 1 to 8 GiB)
/// and slack for the JVM's own overhead and the image mounts (1/32, at least 1 GiB); Snakemake
/// keeps 1/16, 2 to 8 GiB, and [`SNAKEMAKE_JOB_RESERVE_MIB`] a vCPU. Zero when the engine's
/// reserve is all of it (the coordinator then refuses to start).
///
/// A mirror of the coordinator's `single_job_memory` (`docker/coordinator/lib/single_job.sh`),
/// which is what the engine is actually told: change the two together. Both are pinned to the same
/// values by their tests.
pub(crate) fn task_memory_mib(engine: Engine, job: Resources) -> u64 {
    let job_memory_mib = job.memory_mib;
    let reserve = match engine {
        Engine::Nextflow => {
            let heap = (job_memory_mib / 16).clamp(1024, 8192);
            let slack = (job_memory_mib / 32).max(1024);
            heap + slack
        }
        Engine::Snakemake => {
            (job_memory_mib / 16).clamp(2048, 8192)
                + u64::from(job.vcpus) * SNAKEMAKE_JOB_RESERVE_MIB
        }
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
) -> Result<(SingleJobUse, Arch), ServiceError> {
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
/// x86_64 (Batch may place the job on any of them): one that is not x86_64, that carries an
/// accelerator, or that EC2 does not describe. The deploy config refuses such types by name; EC2's
/// description is the authoritative check.
pub(crate) fn choose(
    spec: &SingleJobSpec,
    queue: &SingleJobQueue,
    described: &BTreeMap<String, InstanceTypeInfo>,
    engine: Engine,
) -> Result<(SingleJobUse, Arch), String> {
    let types = described_types(queue, described)?;
    let listed = || queue.instance_types.join(", ");
    let architecture = check_hosts(queue, &types)?;
    let (instance_type, size) = match spec.size()? {
        // `types` is every type of the queue-class (described_types).
        Some(SingleJobSize::Instance(want)) => {
            types.iter().find(|(t, _)| *t == want).map(|(t, info)| (*t, info.size)).ok_or_else(
                || {
                    format!(
                        "instance type `{want}` is not one of single-job queue-class `{}`'s ({})",
                        queue.name,
                        listed()
                    )
                },
            )?
        }
        Some(SingleJobSize::Resources { cpus, memory_gib }) => types
            .iter()
            .map(|(t, info)| (*t, info.size))
            .find(|(_, size)| {
                size.vcpus >= cpus
                    && task_memory_mib(engine, whole_host(*size)) >= u64::from(memory_gib) * 1024
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
    Ok((
        SingleJobUse {
            queue_class: queue.name.clone(),
            instance_type: instance_type.to_string(),
            vcpus: job.vcpus,
            memory_mib: job.memory_mib,
        },
        architecture,
    ))
}

/// `queue`'s instance types as EC2 `described` them, smallest first; refused unless EC2 describes
/// every one: Batch may place the job on any of them, and [`check_hosts`] cannot check a host it
/// knows nothing of (one with more vCPUs would leave room for another run beside it).
fn described_types<'a>(
    queue: &'a SingleJobQueue,
    described: &'a BTreeMap<String, InstanceTypeInfo>,
) -> Result<Vec<(&'a str, &'a InstanceTypeInfo)>, String> {
    let missing: Vec<String> = queue
        .instance_types
        .iter()
        .filter(|t| !described.contains_key(*t))
        .map(|t| format!("`{t}`"))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "EC2 does not describe {} of single-job queue-class `{}` in this deploy's region, so \
             its hosts' vCPUs cannot be checked; each of its instance types must be one EC2 \
             describes",
            missing.join(", "),
            queue.name
        ));
    }
    let mut types: Vec<(&str, &InstanceTypeInfo)> =
        queue.instance_types.iter().map(|t| (t.as_str(), &described[t])).collect();
    types.sort_by_key(|(name, info)| (info.size.vcpus, info.size.memory_mib, *name));
    Ok(types)
}

/// Refuse `queue`'s described `types` (smallest first) unless each is a plain x86_64 type (Batch
/// may place the job on any of them): one that is not x86_64 or carries an accelerator, or a set
/// whose vCPU counts differ. A job takes its host's every vCPU, so on a type with more it would
/// leave room for another run beside it; EC2's counts decide, as a type's name (its size suffix,
/// which the deploy config checks) does not always (`hpc6id.32xlarge` has 64, `x2idn.32xlarge`
/// 128). Types with equal vCPUs and different memory are fine: one job still fills the host.
fn check_hosts(
    queue: &SingleJobQueue,
    types: &[(&str, &InstanceTypeInfo)],
) -> Result<Arch, String> {
    let mut architecture = None;
    for (name, info) in types {
        let host = host_architecture(&info.architectures).map_err(|reason| {
            format!("instance type `{name}` of single-job queue-class `{}`: {reason}", queue.name)
        })?;
        if host != Arch::Amd64 {
            return Err(format!(
                "instance type `{name}` of single-job queue-class `{}` is not x86_64 (EC2 says: \
                 {}); single-job runs need x86_64 hosts",
                queue.name,
                info.architectures.join(", ")
            ));
        }
        // The x86-only policy currently implies homogeneity. Keep the fleet invariant explicit
        // so lifting that policy cannot admit a mixed-architecture queue.
        if architecture.is_some_and(|previous| previous != host) {
            return Err(format!(
                "single-job queue-class `{}` mixes host architectures",
                queue.name
            ));
        }
        architecture = Some(host);
        if !info.accelerators.is_empty() {
            return Err(format!(
                "instance type `{name}` of single-job queue-class `{}` carries an accelerator \
                 ({}); single-job hosts can use none",
                queue.name,
                info.accelerators.join(", ")
            ));
        }
    }
    if types.windows(2).any(|w| w[0].1.size.vcpus != w[1].1.size.vcpus) {
        let counts = types
            .iter()
            .map(|(name, info)| format!("`{name}` ({})", info.size.vcpus))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "single-job queue-class `{}`'s instance types have different vCPU counts ({counts}); \
             a run sized for a smaller host could share a larger one with another run, so each \
             single-job queue-class needs types of one vCPU count",
            queue.name
        ));
    }
    architecture
        .ok_or_else(|| format!("single-job queue-class `{}` has no instance types", queue.name))
}

/// Resolve EC2's list to one supported Linux architecture, ignoring legacy i386.
fn host_architecture(architectures: &[String]) -> Result<Arch, String> {
    let supported: BTreeSet<_> = architectures
        .iter()
        .filter_map(|value| match value.as_str() {
            "x86_64" => Some(Arch::Amd64),
            "arm64" => Some(Arch::Arm64),
            _ => None,
        })
        .collect();
    match supported.len() {
        1 => Ok(*supported.first().expect("one architecture")),
        0 => {
            Err(format!("no supported host architecture (EC2 says: {})", architectures.join(", ")))
        }
        _ => Err(format!("ambiguous host architectures (EC2 says: {})", architectures.join(", "))),
    }
}

/// Recheck, at launch, the hosts of a parked single-job run's queue-class as EC2 describes them
/// now ([`check_hosts`]): a deploy may have changed its instance types while the run waited.
/// `Ok(Err)` is why the run cannot launch; `Err`, that EC2 could not be asked (retry later).
pub(crate) async fn recheck(
    state: &AppState,
    single: &SingleJobUse,
) -> Result<Result<Arch, String>, crate::aws::AwsError> {
    let Some(queue) = state.deploy.single_job_queues.iter().find(|q| q.name == single.queue_class)
    else {
        return Ok(Err(format!(
            "the single-job queue-class `{}` is no longer configured on this deploy",
            single.queue_class
        )));
    };
    let described = state.cloud.describe_instance_types(&queue.instance_types).await?;
    Ok(described_types(queue, &described).and_then(|types| check_hosts(queue, &types)))
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
        choose(spec, queue, described, Engine::Snakemake).map(|(allocation, _)| allocation)
    }

    #[test]
    fn ec2_architectures_resolve_to_one_supported_linux_platform() {
        for (values, expected) in [
            (vec!["x86_64"], Some(Arch::Amd64)),
            (vec!["i386", "x86_64"], Some(Arch::Amd64)),
            (vec!["x86_64", "x86_64"], Some(Arch::Amd64)),
            (vec!["arm64"], Some(Arch::Arm64)),
            (vec![], None),
            (vec!["i386"], None),
            (vec!["arm64_mac"], None),
            (vec!["x86_64_mac"], None),
            (vec!["riscv64"], None),
            (vec!["x86_64", "arm64"], None),
        ] {
            let values: Vec<String> = values.into_iter().map(String::from).collect();
            assert_eq!(host_architecture(&values).ok(), expected, "{values:?}");
        }
    }

    #[test]
    fn the_allocation_carries_its_queue_architecture_without_changing_its_size() {
        let q = queue_class("single", &["c6id.8xlarge"]);
        let (allocation, architecture) =
            choose(&SingleJobSpec::default(), &q, &described(), Engine::Snakemake).unwrap();
        assert_eq!(architecture, Arch::Amd64);
        assert_eq!((allocation.vcpus, allocation.memory_mib), (32, 61_184));
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

    /// The (engine, job vCPUs, job memory MiB) -> task memory MiB rows the coordinator's
    /// `single_job_memory` is pinned to as well (`docker/coordinator/tests/test_lib.py`).
    #[test]
    fn the_engine_keeps_its_reserve_of_the_jobs_memory_as_the_coordinator_does() {
        for (engine, vcpus, memory_mib, tasks) in [
            (Engine::Nextflow, 2, 8_192, 6_144),
            (Engine::Nextflow, 32, 61_184, 55_448),
            (Engine::Nextflow, 8, 65_536, 59_392),
            (Engine::Nextflow, 64, 524_288, 499_712),
            (Engine::Snakemake, 2, 8_192, 5_824),
            (Engine::Snakemake, 32, 61_184, 52_240),
            (Engine::Snakemake, 8, 65_536, 60_160),
            (Engine::Snakemake, 64, 524_288, 505_856),
        ] {
            let job = Resources { vcpus, memory_mib };
            assert_eq!(task_memory_mib(engine, job), tasks, "{engine} {vcpus} {memory_mib}");
        }
        // A job smaller than the engine's reserve leaves nothing, never underflows.
        let tiny = Resources { vcpus: 1, memory_mib: 1_024 };
        assert_eq!(task_memory_mib(Engine::Snakemake, tiny), 0);
        assert_eq!(task_memory_mib(Engine::Nextflow, tiny), 0);
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
        // A c6id.8xlarge (64 GiB, 32 vCPUs) leaves its Snakemake tasks 52240 MiB, 51 GiB: enough
        // for 51, not for 52, nor for the 64 its nominal memory would suggest.
        assert_eq!(pick(32, 51).unwrap().instance_type, "c6id.8xlarge");
        assert_eq!(pick(32, 52).unwrap().instance_type, "m6id.8xlarge");
        assert_eq!(pick(32, 64).unwrap().instance_type, "m6id.8xlarge");
        assert_eq!(pick(16, 100).unwrap().instance_type, "m6id.8xlarge");
        assert_eq!(pick(32, 200).unwrap().instance_type, "r6id.8xlarge");
        let err = pick(64, 64).unwrap_err();
        assert!(err.contains("no instance type"), "{err}");
        // A Nextflow run's engine keeps less (no Snakemake per job): a c6id.8xlarge leaves its
        // tasks 55448 MiB.
        let nf = |gib| {
            choose(&spec(None, Some(32), Some(gib)), &q, &described(), Engine::Nextflow).unwrap().0
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

    /// Batch may place the job on any type of the queue-class, so one whose vCPUs EC2 does not
    /// describe could hold another run beside it: the queue-class is refused, however the run is
    /// sized.
    #[test]
    fn a_type_ec2_does_not_describe_refuses_the_queue_class_naming_it() {
        let q = queue_class("single", &["c6id.8xlarge", "z9.huge", "z9.tiny"]);
        for spec in [
            SingleJobSpec::default(),
            spec(Some("c6id.8xlarge"), None, None),
            spec(Some("z9.huge"), None, None),
            spec(None, Some(8), Some(16)),
        ] {
            let err = choose_sm(&spec, &q, &described()).unwrap_err();
            assert!(
                err.contains("EC2 does not describe `z9.huge`, `z9.tiny`")
                    && err.contains("`single`"),
                "{err}"
            );
        }
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
    fn an_ambiguous_host_architecture_is_refused_even_when_it_includes_x86_64() {
        let q = queue_class("single", &["c6id.8xlarge"]);
        for architectures in [["x86_64", "arm64"], ["arm64", "x86_64"]] {
            let mut types = described();
            types.get_mut("c6id.8xlarge").unwrap().architectures =
                architectures.into_iter().map(String::from).collect();
            let result = choose_sm(&SingleJobSpec::default(), &q, &types);
            assert!(result.is_err(), "ambiguous host was admitted: {result:?}");
            let reason = result.unwrap_err();
            assert!(reason.contains("c6id.8xlarge") && reason.contains("architecture"), "{reason}");
        }
    }

    #[test]
    fn legacy_i386_does_not_prevent_x86_64_host_admission() {
        let q = queue_class("single", &["c6id.8xlarge"]);
        let mut types = described();
        types.get_mut("c6id.8xlarge").unwrap().architectures = vec!["i386".into(), "x86_64".into()];
        assert_eq!(
            choose_sm(&SingleJobSpec::default(), &q, &types).unwrap().instance_type,
            "c6id.8xlarge"
        );
    }

    /// Same size suffix, different machines: a job sized for the smaller host's vCPUs fits twice
    /// on the larger one, so two runs could share it. EC2's vCPU counts decide, not the names;
    /// differing memory with equal vCPUs is fine (one job takes every vCPU).
    #[test]
    fn a_queue_class_whose_types_differ_in_vcpus_is_refused_naming_them() {
        let mut types = described();
        for (name, vcpus, memory_mib) in
            [("hpc6id.32xlarge", 64, 1_048_576), ("x2idn.32xlarge", 128, 2_097_152)]
        {
            let size = Resources { vcpus, memory_mib };
            types.insert(name.into(), InstanceTypeInfo::x86_64(size));
        }
        let q = queue_class("single", &["hpc6id.32xlarge", "x2idn.32xlarge"]);
        for spec in [SingleJobSpec::default(), spec(Some("x2idn.32xlarge"), None, None)] {
            let err = choose_sm(&spec, &q, &types).unwrap_err();
            assert!(
                err.contains("`hpc6id.32xlarge` (64)") && err.contains("`x2idn.32xlarge` (128)"),
                "{err}"
            );
        }
        let q = queue_class("single", &["c6id.8xlarge", "m6id.8xlarge", "r6id.8xlarge"]);
        assert!(choose_sm(&SingleJobSpec::default(), &q, &types).is_ok());
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
