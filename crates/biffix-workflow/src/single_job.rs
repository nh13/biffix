//! Single-job execution: a run that executes as one AWS Batch job on one host, its engine running
//! every task locally in the task's own image (nested apptainer), instead of one Batch job per task.
//!
//! A version allows it, with an optional default size, in `biffix.yaml` (`single_job:`, frozen into
//! the FormSchema at registration); a run selects it at submit and may override the size. Both
//! spell it as a [`SingleJobSpec`]: the run's spec is laid over the version's ([`SingleJobSpec::over`])
//! and the result names the single-job queue-class and [`SingleJobSize`] the backend resolves to an
//! exact instance type.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A single-job run's size: an exact instance type, or the vCPUs and memory the run needs (the
/// smallest instance type of the queue-class that has both is used).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SingleJobSize {
    /// An exact instance type (`c6id.8xlarge`).
    Instance(String),
    /// At least this many vCPUs and GiB of memory.
    Resources { cpus: u32, memory_gib: u32 },
}

/// How a run executes as one job: which single-job queue-class and what size. Every field is
/// optional; a size is either `instance` or both `cpus` and `memory_gib`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SingleJobSpec {
    /// The single-job queue-class (a deploy's `single_job: true` queue-class) to run on; absent ⇒
    /// the deploy's first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_class: Option<String>,
    /// An exact instance type of that queue-class (`c6id.8xlarge`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The vCPUs the run needs (with `memory_gib`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// The memory the run needs, in GiB (with `cpus`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gib: Option<u32>,
}

impl SingleJobSpec {
    /// Whether the spec names a size at all (any of `instance`, `cpus`, `memory_gib`).
    fn has_size(&self) -> bool {
        self.instance.is_some() || self.cpus.is_some() || self.memory_gib.is_some()
    }

    /// The size the spec names, checked: `None` when it names none; an error for an instance type
    /// given with `cpus`/`memory_gib`, only one of `cpus` and `memory_gib`, a zero, or an instance
    /// type that is not `<class>.<size>`.
    pub fn size(&self) -> Result<Option<SingleJobSize>, String> {
        match (&self.instance, self.cpus, self.memory_gib) {
            (None, None, None) => Ok(None),
            (Some(instance), None, None) => {
                check_instance_type(instance)?;
                Ok(Some(SingleJobSize::Instance(instance.clone())))
            }
            (Some(_), _, _) => {
                Err("give either an instance type or cpus and memory_gib, not both".into())
            }
            (None, Some(cpus), Some(memory_gib)) => {
                if cpus == 0 || memory_gib == 0 {
                    return Err("cpus and memory_gib must each be at least 1".into());
                }
                Ok(Some(SingleJobSize::Resources { cpus, memory_gib }))
            }
            (None, _, _) => Err("cpus and memory_gib must be given together".into()),
        }
    }

    /// This spec (a run's) laid over `version`'s (its default): the run's queue-class, else the
    /// version's; the run's size when it names one, else the version's, unless the run names a
    /// different queue-class (the version's size is for its own queue-class, whose types another
    /// may not run: the other's smallest type is used instead). A run that names only `cpus` does
    /// not inherit the version's `memory_gib`: a size is taken whole from one side.
    ///
    /// The queue-classes are compared by name, so a version that names none (it means the
    /// deploy's default, which this does not know) must have that default filled in first:
    /// otherwise a run naming the default by name counts as another queue-class, and loses the
    /// version's size.
    pub fn over(&self, version: &SingleJobSpec) -> SingleJobSpec {
        let same_class = self.queue_class.is_none() || self.queue_class == version.queue_class;
        let sized = if self.has_size() || !same_class { self } else { version };
        SingleJobSpec {
            queue_class: self.queue_class.clone().or_else(|| version.queue_class.clone()),
            instance: sized.instance.clone(),
            cpus: sized.cpus,
            memory_gib: sized.memory_gib,
        }
    }
}

/// Refuse an instance type that is not `<class>.<size>` (lowercase letters, digits and `-`).
fn check_instance_type(instance: &str) -> Result<(), String> {
    let part_ok = |p: &str| {
        !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    match instance.split_once('.') {
        Some((class, size)) if part_ok(class) && part_ok(size) => Ok(()),
        _ => Err(format!("`{instance}` is not an exact instance type like `c6id.8xlarge`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(instance: Option<&str>, cpus: Option<u32>, memory_gib: Option<u32>) -> SingleJobSpec {
        SingleJobSpec {
            queue_class: None,
            instance: instance.map(str::to_string),
            cpus,
            memory_gib,
        }
    }

    #[test]
    fn a_size_is_an_instance_type_or_cpus_with_memory() {
        assert_eq!(spec(None, None, None).size(), Ok(None));
        assert_eq!(
            spec(Some("c6id.8xlarge"), None, None).size(),
            Ok(Some(SingleJobSize::Instance("c6id.8xlarge".into())))
        );
        assert_eq!(
            spec(None, Some(32), Some(64)).size(),
            Ok(Some(SingleJobSize::Resources { cpus: 32, memory_gib: 64 }))
        );
    }

    #[test]
    fn a_partial_mixed_or_zero_size_is_refused() {
        for bad in [
            spec(Some("c6id.8xlarge"), Some(32), None),
            spec(Some("c6id.8xlarge"), None, Some(64)),
            spec(None, Some(32), None),
            spec(None, None, Some(64)),
            spec(None, Some(0), Some(64)),
            spec(None, Some(32), Some(0)),
            spec(Some("c6id"), None, None),
            spec(Some("C6ID.8XLARGE"), None, None),
            spec(Some("c6id.8xlarge --x"), None, None),
            spec(Some(".8xlarge"), None, None),
        ] {
            assert!(bad.size().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_run_spec_overrides_the_versions_queue_class_and_whole_size() {
        let version = SingleJobSpec {
            queue_class: Some("single".into()),
            instance: Some("c6id.8xlarge".into()),
            ..Default::default()
        };
        // Nothing given: the version's.
        assert_eq!(SingleJobSpec::default().over(&version), version);
        // A size replaces the version's whole size, not one field of it.
        let run = spec(None, Some(16), Some(32));
        assert_eq!(
            run.over(&version),
            SingleJobSpec {
                queue_class: Some("single".into()),
                instance: None,
                cpus: Some(16),
                memory_gib: Some(32),
            }
        );
        // Another queue-class: not the version's size, which is for its own.
        let run = SingleJobSpec { queue_class: Some("big".into()), ..Default::default() };
        assert_eq!(run.over(&version).queue_class.as_deref(), Some("big"));
        assert_eq!(run.over(&version).instance, None);
        // The version's own queue-class named again: its size still applies.
        let run = SingleJobSpec { queue_class: Some("single".into()), ..Default::default() };
        assert_eq!(run.over(&version).instance.as_deref(), Some("c6id.8xlarge"));
        // A version naming no queue-class is compared by name: the caller fills in the deploy's
        // default first, after which a run naming the default keeps the version's size.
        let implicit = SingleJobSpec { queue_class: None, ..version.clone() };
        assert_eq!(run.over(&implicit).instance, None);
        let filled = SingleJobSpec { queue_class: Some("single".into()), ..implicit };
        assert_eq!(run.over(&filled).instance.as_deref(), Some("c6id.8xlarge"));
    }
}
