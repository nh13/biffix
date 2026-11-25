//! The advisories `register` prints for a Snakemake workflow's `biffix.yaml` `queues.by_name`,
//! from the rules [`biffix_workflow::scan::resources`] read. Two things it catches before a run:
//! an entry for a rule the workflow does not define assigns nothing, so the tasks it was meant for
//! run on the default queue-class; and the rules of one `group:` run as one AWS Batch job, on one
//! queue, so an assignment that splits them (some rules assigned, or different queue-classes)
//! fails every such group job at submit. When the scan cannot see every rule name (a dynamic
//! `include:`, a module, an unnamed rule or a `name:` directive), or every `group:` (one that is
//! not a plain string), that check says nothing; groups from `--groups` or a profile's `groups:`
//! are not seen either. A Nextflow workflow's process names are not known before it runs, so its
//! selectors are not checked: each task's provenance records the queue it ran on. It never fails
//! the command.

use std::collections::{BTreeMap, BTreeSet};

use biffix_workflow::queues::QueueAssignment;
use biffix_workflow::scan::resources::ResourceScan;

/// Print (to stderr) the warnings for `queues` against the Snakemake workflow `scan` read.
pub(crate) fn warn_queue_assignment(scan: &ResourceScan, queues: &QueueAssignment) {
    if queues.by_name.is_empty() {
        return;
    }
    for warning in warnings(scan.rule_names.as_ref(), scan.rule_groups.as_ref(), queues) {
        teprintln!("[biffix] {warning}");
    }
}

/// The warnings for `queues` against the workflow's rules `rule_names` and their groups
/// `rule_groups` (rule → group); `None` for what the scan did not see in full.
fn warnings(
    rule_names: Option<&BTreeSet<String>>,
    rule_groups: Option<&BTreeMap<String, String>>,
    queues: &QueueAssignment,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(rule_names) = rule_names {
        let unmatched: Vec<String> = queues
            .by_name
            .keys()
            .filter(|name| !rule_names.contains(*name))
            .map(|name| format!("`{name}`"))
            .collect();
        if !unmatched.is_empty() {
            out.push(format!(
                "warning: biffix.yaml `queues.by_name` names rule(s) the workflow does not \
                 define: {}. Such an entry assigns nothing; check the spelling against the \
                 Snakefile's rules.",
                unmatched.join(", ")
            ));
        }
    }
    if let Some(rule_groups) = rule_groups {
        let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (rule, group) in rule_groups {
            groups.entry(group.as_str()).or_default().push(rule.as_str());
        }
        for (group, rules) in groups {
            let assigned: Vec<Option<&str>> =
                rules.iter().map(|r| queues.by_name.get(*r).map(String::as_str)).collect();
            if assigned.iter().all(Option::is_none) || assigned.windows(2).all(|w| w[0] == w[1]) {
                continue;
            }
            let listed: Vec<String> = rules
                .iter()
                .zip(&assigned)
                .map(|(rule, qc)| format!("{rule} -> {}", qc.unwrap_or("(not assigned)")))
                .collect();
            out.push(format!(
                "warning: biffix.yaml `queues.by_name` splits the rules of `group: {group}` ({}). \
                 A group runs as one AWS Batch job, on one queue, so every such group job would \
                 fail at submit; give every rule of the group the same queue-class, or none of \
                 them.",
                listed.join(", ")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_name(pairs: &[(&str, &str)]) -> QueueAssignment {
        QueueAssignment {
            by_name: pairs.iter().map(|(n, qc)| (n.to_string(), qc.to_string())).collect(),
            ..QueueAssignment::default()
        }
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn groups(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(r, g)| (r.to_string(), g.to_string())).collect()
    }

    #[test]
    fn a_name_the_workflow_does_not_define_is_named() {
        let rules = set(&["align", "call"]);
        let ok = by_name(&[("align", "gpu"), ("call", "gpu")]);
        assert!(warnings(Some(&rules), None, &ok).is_empty());
        assert_eq!(
            warnings(
                Some(&rules),
                None,
                &by_name(&[("algin", "gpu"), ("call", "gpu"), ("zzz", "gpu")])
            ),
            vec![
                "warning: biffix.yaml `queues.by_name` names rule(s) the workflow does not define: \
                 `algin`, `zzz`. Such an entry assigns nothing; check the spelling against the \
                 Snakefile's rules."
                    .to_string()
            ]
        );
    }

    #[test]
    fn nothing_is_said_when_not_every_rule_was_seen() {
        assert!(warnings(None, None, &by_name(&[("algin", "gpu")])).is_empty());
    }

    #[test]
    fn a_group_split_across_queue_classes_is_named() {
        let g = groups(&[("a", "g1"), ("b", "g1"), ("c", "g2"), ("d", "g2"), ("e", "g3")]);
        // g1 all on one queue-class, g2 untouched, g3 a single rule: nothing to say.
        let ok = by_name(&[("a", "gpu"), ("b", "gpu"), ("e", "cpu")]);
        assert!(warnings(None, Some(&g), &ok).is_empty());
        // g1 partly assigned, g2 on two queue-classes.
        let split = by_name(&[("a", "gpu"), ("c", "gpu"), ("d", "cpu")]);
        assert_eq!(
            warnings(None, Some(&g), &split),
            vec![
                "warning: biffix.yaml `queues.by_name` splits the rules of `group: g1` (a -> gpu, \
                 b -> (not assigned)). A group runs as one AWS Batch job, on one queue, so every \
                 such group job would fail at submit; give every rule of the group the same \
                 queue-class, or none of them."
                    .to_string(),
                "warning: biffix.yaml `queues.by_name` splits the rules of `group: g2` (c -> gpu, \
                 d -> cpu). A group runs as one AWS Batch job, on one queue, so every such group \
                 job would fail at submit; give every rule of the group the same queue-class, or \
                 none of them."
                    .to_string(),
            ]
        );
    }
}
