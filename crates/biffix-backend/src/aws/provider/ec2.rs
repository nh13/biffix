//! EC2 ([`InstanceTypes`]): DescribeInstanceTypes, for the instance types single-job runs are
//! sized from.

use std::collections::BTreeMap;

use async_trait::async_trait;
use aws_sdk_ec2::types::{Filter, InstanceTypeInfo as Ec2InstanceTypeInfo};

use super::errors::OrClassify;
use super::AwsCloudProvider;
use crate::aws::ec2::{InstanceTypeInfo, InstanceTypes, Resources};
use crate::aws::AwsError;

#[async_trait]
impl InstanceTypes for AwsCloudProvider {
    async fn describe_instance_types(
        &self,
        instance_types: &[String],
    ) -> Result<BTreeMap<String, InstanceTypeInfo>, AwsError> {
        let mut described = BTreeMap::new();
        if instance_types.is_empty() {
            return Ok(described);
        }
        // A filter, not `InstanceTypes`: EC2 refuses the whole request when one listed type does
        // not exist in the region, where a filter just matches none for it (absent from the map).
        let filter = Filter::builder()
            .name("instance-type")
            .set_values(Some(instance_types.to_vec()))
            .build();
        let mut pages = self.ec2.describe_instance_types().filters(filter).into_paginator().send();
        while let Some(page) = pages.next().await {
            let page = page.or_classify(AwsError::Ec2)?;
            for info in page.instance_types() {
                if let Some((kind, info)) = instance_type_info(info) {
                    described.insert(kind, info);
                }
            }
        }
        Ok(described)
    }
}

/// One described type as biffix reads it; `None` for one EC2 describes without a size.
fn instance_type_info(info: &Ec2InstanceTypeInfo) -> Option<(String, InstanceTypeInfo)> {
    let kind = info.instance_type()?.as_str().to_string();
    let vcpus = u32::try_from(info.v_cpu_info()?.default_v_cpus()?).ok()?;
    let memory_mib = u64::try_from(info.memory_info()?.size_in_mib()?).ok()?;
    let architectures = info
        .processor_info()
        .map(|p| p.supported_architectures().iter().map(|a| a.as_str().to_string()).collect())
        .unwrap_or_default();
    let accelerators = [
        ("GPU", info.gpu_info().is_some_and(|g| !g.gpus().is_empty())),
        ("FPGA", info.fpga_info().is_some_and(|f| !f.fpgas().is_empty())),
        (
            "inference accelerator",
            info.inference_accelerator_info().is_some_and(|i| !i.accelerators().is_empty()),
        ),
        (
            "media accelerator",
            info.media_accelerator_info().is_some_and(|m| !m.accelerators().is_empty()),
        ),
        ("Neuron", info.neuron_info().is_some_and(|n| !n.neuron_devices().is_empty())),
    ]
    .into_iter()
    .filter(|(_, has)| *has)
    .map(|(name, _)| name.to_string())
    .collect();
    Some((
        kind,
        InstanceTypeInfo { size: Resources { vcpus, memory_mib }, architectures, accelerators },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_ec2::types::{
        ArchitectureType, GpuDeviceInfo, GpuInfo, InstanceType, MemoryInfo, ProcessorInfo, VCpuInfo,
    };

    fn described(arch: ArchitectureType, gpu: bool) -> Ec2InstanceTypeInfo {
        let mut info = Ec2InstanceTypeInfo::builder()
            .instance_type(InstanceType::from("c6id.8xlarge"))
            .v_cpu_info(VCpuInfo::builder().default_v_cpus(32).build())
            .memory_info(MemoryInfo::builder().size_in_mib(65_536).build())
            .processor_info(ProcessorInfo::builder().supported_architectures(arch).build());
        if gpu {
            info = info.gpu_info(GpuInfo::builder().gpus(GpuDeviceInfo::builder().build()).build());
        }
        info.build()
    }

    #[test]
    fn a_described_type_reads_its_size_architecture_and_accelerators() {
        let (kind, info) = instance_type_info(&described(ArchitectureType::X8664, false)).unwrap();
        assert_eq!(kind, "c6id.8xlarge");
        assert_eq!(info, InstanceTypeInfo::x86_64(Resources { vcpus: 32, memory_mib: 65_536 }));
        let (_, info) = instance_type_info(&described(ArchitectureType::Arm64, true)).unwrap();
        assert_eq!(info.architectures, ["arm64"]);
        assert_eq!(info.accelerators, ["GPU"]);
        // No size: not one a run can be sized from.
        let bare = Ec2InstanceTypeInfo::builder().instance_type(InstanceType::from("x.y")).build();
        assert_eq!(instance_type_info(&bare), None);
    }
}
