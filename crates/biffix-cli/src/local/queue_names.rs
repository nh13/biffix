//! The advisory `register` prints for a Snakemake workflow whose `biffix.yaml` `queues.by_name`
//! names a rule the workflow does not define: such an entry assigns nothing, so the tasks it was
//! meant for run on the default queue-class. Rule names are read from the source by
//! [`biffix_workflow::scan::resources`]; when it cannot see every rule name (a dynamic
//! `include:`, a module, an unnamed rule or a `name:` directive), nothing is said. A Nextflow workflow's process names are not known before it runs, so
//! its selectors are not checked: each task's provenance records the queue it ran on. It never
//! fails the command.

use std::collections::BTreeSet;
use std::path::Path;

use biffix_workflow::queues::QueueAssignment;
use biffix_workflow::scan::resources::scan_snakemake_resources;

use crate::local::source::SourceFiles;

/// Print (to stderr) the warning for the Snakemake workflow at `root` (source `files`, run from
/// `entrypoint`), if `queues.by_name` names a rule it does not define.
pub(crate) fn warn_unmatched_rules(
    root: &Path,
    files: &SourceFiles,
    entrypoint: &str,
    queues: &QueueAssignment,
) {
    if queues.by_name.is_empty() {
        return;
    }
    let paths = files.paths();
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let scan = scan_snakemake_resources(&paths, read, entrypoint, None);
    if let Some(warning) = warning_text(scan.rule_names.as_ref(), queues) {
        eprintln!("[biffix] {warning}");
    }
}

/// The warning for `queues` against the workflow's rules `rule_names` (`None`: not all known).
fn warning_text(rule_names: Option<&BTreeSet<String>>, queues: &QueueAssignment) -> Option<String> {
    let rule_names = rule_names?;
    let unmatched: Vec<String> = queues
        .by_name
        .keys()
        .filter(|name| !rule_names.contains(*name))
        .map(|name| format!("`{name}`"))
        .collect();
    if unmatched.is_empty() {
        return None;
    }
    Some(format!(
        "warning: biffix.yaml `queues.by_name` names rule(s) the workflow does not define: {}. \
         Such an entry assigns nothing; check the spelling against the Snakefile's rules.",
        unmatched.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_name(names: &[&str]) -> QueueAssignment {
        QueueAssignment {
            by_name: names.iter().map(|n| (n.to_string(), "gpu".to_string())).collect(),
            ..QueueAssignment::default()
        }
    }

    #[test]
    fn a_name_the_workflow_does_not_define_is_named() {
        let rules: BTreeSet<String> = ["align".to_string(), "call".into()].into();
        assert_eq!(warning_text(Some(&rules), &by_name(&["align", "call"])), None);
        assert_eq!(
            warning_text(Some(&rules), &by_name(&["algin", "call", "zzz"])).unwrap(),
            "warning: biffix.yaml `queues.by_name` names rule(s) the workflow does not define: \
             `algin`, `zzz`. Such an entry assigns nothing; check the spelling against the \
             Snakefile's rules."
        );
    }

    #[test]
    fn nothing_is_said_when_not_every_rule_was_seen() {
        assert_eq!(warning_text(None, &by_name(&["algin"])), None);
    }
}
