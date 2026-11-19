//! EC2: the instance types a single-job run is sized from ([`InstanceTypes`]).

use std::collections::BTreeMap;

use async_trait::async_trait;

use super::AwsError;

/// vCPUs and memory: an instance type's size as EC2 describes it, or what a single-job run's one
/// job is given on such a host (`crate::runs::single_job::whole_host`), sent as the job's resource
/// requirements and to its coordinator (`BIFFIX_SINGLE_JOB_CPUS`, `BIFFIX_SINGLE_JOB_MEMORY_MIB`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resources {
    /// vCPUs (an instance type's default vCPUs).
    pub vcpus: u32,
    /// Memory, MiB (an instance type's nominal size; a host registers somewhat less with ECS).
    pub memory_mib: u64,
}

/// An EC2 instance type as a single-job run needs it described: its size, the processor
/// architectures it supports, and the accelerators it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceTypeInfo {
    /// Its default vCPUs and nominal memory.
    pub size: Resources,
    /// The architectures EC2 says it supports (`x86_64`, `arm64`, `i386`, …).
    pub architectures: Vec<String>,
    /// The kinds of accelerator it carries (`GPU`, `FPGA`, `inference accelerator`, `media
    /// accelerator`, `Neuron`); empty for none.
    pub accelerators: Vec<String>,
}

impl InstanceTypeInfo {
    /// A plain x86_64 type of `size`, with no accelerator.
    pub fn x86_64(size: Resources) -> Self {
        Self { size, architectures: vec!["x86_64".into()], accelerators: Vec::new() }
    }
}

/// The EC2 instance types the deploy's single-job queue-classes run (see the module docs).
#[async_trait]
pub trait InstanceTypes: Send + Sync {
    /// Each of `instance_types` (exact EC2 types, those of a single-job queue-class) by type, as
    /// EC2 describes it. A type EC2 does not know is absent from the map.
    async fn describe_instance_types(
        &self,
        instance_types: &[String],
    ) -> Result<BTreeMap<String, InstanceTypeInfo>, AwsError>;
}
