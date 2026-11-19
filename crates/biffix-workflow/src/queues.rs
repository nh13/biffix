//! Which worker queue-class each task runs on: the `queues:` block of `biffix.yaml`.
//!
//! ```yaml
//! queues:
//!   default: small-spot            # every task no selector below picks
//!   by_label:                      # Nextflow only: a process `label`
//!     process_high: large-spot
//!   by_name:                       # a Nextflow process or a Snakemake rule, by name
//!     ALIGN: large-spot
//! ```
//!
//! The values are queue-class names, never Batch queue names or ARNs, so a workflow stays portable
//! across deployments: the coordinator resolves each to the deployment's own queue. How each
//! engine applies the assignment:
//!
//! - **Nextflow**: `default` is `process.queue`, each `by_label` entry a
//!   `process { withLabel: '<label>' { queue = … } }` and each `by_name` entry a
//!   `process { withName: '<name>' { queue = … } }`, written to the config file biffix adds with
//!   `-c`. Nextflow's own selector priority decides between them (a generic setting, then the
//!   process definition, then `withLabel`, then `withName`), and that file is read after the
//!   pipeline's, so for the same selector biffix's value wins.
//! - **Snakemake**: `default` is the executor's `--aws-batch-job-queue` (a rule's own
//!   `batch_queue` resource still wins), and the `by_name` entries the executor's rule queue map
//!   (`--aws-batch-rule-queues`), which outranks a rule's own `batch_queue` and, unlike
//!   `--set-resources`, leaves the workflow profile's `set-resources` in place. Snakemake rules
//!   have no labels, so `by_label` is refused. Rules that share a `group:` run as one AWS Batch job,
//!   on one queue, so they must share a queue-class: assign every rule of a group the same one,
//!   or none of them.
//!
//! Every selector is an exact name, never a pattern. Nextflow reads `withLabel` and `withName` as
//! regular expressions, so its labels and names are held to grammars with no regex
//! metacharacter. Snakemake's rule queue map is matched exactly, and Snakemake names a rule with
//! any string (a `name:` directive's, or an unnamed rule's number), so a rule name is any
//! non-empty string with no control character.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};

use crate::engines::Engine;

/// The most bytes the `by_label` and `by_name` entries may take as JSON: they travel in the
/// coordinator's environment, which AWS Batch caps (with the rest of its container overrides) at
/// 8 KiB, most of it left for the run's parameters.
const MAX_QUEUE_SELECTOR_BYTES: usize = 2048;

/// A Nextflow process name: an identifier (Groovy's, Unicode letters included), or identifiers
/// joined by `:` (a fully qualified name). No character of it is a regex metacharacter.
static PROCESS_NAME: LazyLock<Regex> = LazyLock::new(|| {
    let identifier = r"[\p{XID_Start}_]\p{XID_Continue}*";
    Regex::new(&format!("^{identifier}(?::{identifier})*$")).expect("valid")
});

/// A label as Nextflow's `label` directive accepts it (`ProcessBuilder.isValidLabel`): a letter,
/// then letters, digits or `_`, ending in a letter or digit; or two of those as `key=value`.
static NEXTFLOW_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    let word = r"[A-Za-z](?:[A-Za-z0-9_]*[A-Za-z0-9])?";
    Regex::new(&format!("^{word}(?:={word})?$")).expect("valid")
});

/// The `queues:` block of `biffix.yaml`: the queue-class every task runs on by default, and the
/// tasks that run elsewhere, by label (Nextflow) or by name. Recorded on the version as given.
///
/// Unknown keys are kept here ([`Self::unknown`]), so a version stored by a newer backend (with a
/// key added since) still reads, and the key still reaches the coordinator, which refuses an
/// assignment it cannot apply rather than run those tasks on the default queue. `biffix.yaml`
/// refuses them, through [`BiffixManifest`](crate::manifest), and so do registration and
/// re-registration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueAssignment {
    /// The queue-class tasks run on when no selector picks one; absent, the version's first
    /// `--queue-class`, else the deployment's first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Nextflow only: process label → queue-class.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_label: BTreeMap<String, String>,
    /// Process (Nextflow) or rule (Snakemake) name → queue-class. A Nextflow name is the process's
    /// own name, its alias, or its fully qualified name (`WORKFLOW:SUB:PROCESS`), as `withName`
    /// matches them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_name: BTreeMap<String, String>,
    /// Keys this backend does not know, as a newer one stored them: kept so they travel to the
    /// coordinator with the selectors (see above). Never set by registration, which refuses them.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// [`QueueAssignment`] as `biffix.yaml` must spell it: an unknown (likely misspelled) key is
/// refused rather than silently assigning nothing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestQueueAssignment {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    by_label: BTreeMap<String, String>,
    #[serde(default)]
    by_name: BTreeMap<String, String>,
}

/// Read `biffix.yaml`'s `queues:`, refusing unknown keys (for `#[serde(deserialize_with)]`).
pub(crate) fn deserialize_manifest<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<QueueAssignment>, D::Error> {
    Ok(Option::<ManifestQueueAssignment>::deserialize(d)?.map(|q| QueueAssignment {
        default: q.default,
        by_label: q.by_label,
        by_name: q.by_name,
        unknown: BTreeMap::new(),
    }))
}

/// Why a `queues:` block is not accepted. The messages name the block as `queues`; a caller that
/// read it from `biffix.yaml` says so.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueueAssignmentError {
    #[error(
        "`queues.by_label` applies only to Nextflow workflows (Snakemake rules have no labels); \
         assign rules with `queues.by_name`"
    )]
    LabelsNeedNextflow,
    #[error(
        "`queues.by_label`: `{0}` is not a Nextflow label (a letter, then letters, digits or `_`, \
         ending in a letter or digit; or two of those as `key=value`)"
    )]
    BadLabel(String),
    #[error(
        "`queues.by_name`: `{0}` is not a Nextflow process name (an identifier, or identifiers \
         joined by `:` for a fully qualified name)"
    )]
    BadProcessName(String),
    #[error(
        "`queues.by_name`: `{0}` is not a Snakemake rule name (a non-empty name with no control \
         character)"
    )]
    BadRuleName(String),
    /// Keys this version of biffix does not know (only a stored assignment can carry them).
    #[error("`queues` has keys this biffix does not know: {}", .0.join(", "))]
    UnknownKeys(Vec<String>),
    #[error(
        "`queues`: its selectors (the `by_label` and `by_name` entries, labels, names and \
         queue-classes) take {0} bytes as JSON, more than the {MAX_QUEUE_SELECTOR_BYTES} a run can \
         carry; select fewer tasks (one label can cover many processes)"
    )]
    TooLarge(usize),
    #[error(
        "`queues` names queue-class(es) the version does not list: {}; register with \
         `--queue-class` for each queue-class the version uses{}",
        .missing.join(", "),
        listed_note(.listed)
    )]
    NotInVersion { missing: Vec<String>, listed: Vec<String> },
}

/// A Snakemake rule name as the rule queue map can carry it: Snakemake names a rule with any
/// string, so any non-empty one with no control character (which no Snakefile can spell).
fn is_rule_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(char::is_control)
}

fn listed_note(listed: &[String]) -> String {
    if listed.is_empty() {
        " (it lists none)".to_string()
    } else {
        format!(" (it lists: {})", listed.join(", "))
    }
}

impl QueueAssignment {
    /// Whether the block assigns nothing (absent, or every key empty; a key this backend does not
    /// know counts as something).
    pub fn is_empty(&self) -> bool {
        self.default.is_none()
            && self.by_label.is_empty()
            && self.by_name.is_empty()
            && self.unknown.is_empty()
    }

    /// The queue-class a task no selector picks runs on: the block's `default`, else the first of
    /// the version's queue-classes `listed` (`None`: the deployment's first).
    pub fn default_queue_class<'a>(&'a self, listed: &'a [String]) -> Option<&'a str> {
        self.default.as_deref().or(listed.first().map(String::as_str))
    }

    /// Check the block for a version of `engine` that lists the queue-classes `listed`
    /// (`--queue-class`): labels only for Nextflow, every label and name one the engine's
    /// selectors match exactly, within the size limits, and every queue-class one of `listed`
    /// (the version's list is what it declares it uses, and the block assigns within it; the
    /// server checks `listed` against the deploy's menu).
    pub fn validate(&self, engine: Engine, listed: &[String]) -> Result<(), QueueAssignmentError> {
        self.check_selectors(engine)?;
        self.check_listed(listed)
    }

    /// The selectors alone (`default` left out): what the coordinator writes into the engine's
    /// config, the default being sent as the run's queue-class.
    pub fn selectors(&self) -> QueueAssignment {
        QueueAssignment { default: None, ..self.clone() }
    }

    /// Every `(where, queue-class)` the block names, `where` being its key under `queues`
    /// (`default`, `by_label.<label>`, `by_name.<name>`), in that order.
    fn entries(&self) -> Vec<(String, &str)> {
        let default = self.default.iter().map(|qc| ("default".to_string(), qc.as_str()));
        let labels = self.by_label.iter().map(|(l, qc)| (format!("by_label.{l}"), qc.as_str()));
        let names = self.by_name.iter().map(|(n, qc)| (format!("by_name.{n}"), qc.as_str()));
        default.chain(labels).chain(names).collect()
    }

    /// The selectors' size, then labels only for Nextflow, then each label and name. The
    /// queue-class values are settled by [`Self::check_listed`] (and, through the version's list,
    /// the deploy's menu).
    fn check_selectors(&self, engine: Engine) -> Result<(), QueueAssignmentError> {
        if !self.unknown.is_empty() {
            return Err(QueueAssignmentError::UnknownKeys(self.unknown.keys().cloned().collect()));
        }
        let bytes = serde_json::to_string(&self.selectors())
            .expect("a map of strings always serializes")
            .len();
        if bytes > MAX_QUEUE_SELECTOR_BYTES {
            return Err(QueueAssignmentError::TooLarge(bytes));
        }
        if !self.by_label.is_empty() && engine != Engine::Nextflow {
            return Err(QueueAssignmentError::LabelsNeedNextflow);
        }
        for label in self.by_label.keys() {
            if !NEXTFLOW_LABEL.is_match(label) {
                return Err(QueueAssignmentError::BadLabel(label.clone()));
            }
        }
        for name in self.by_name.keys() {
            match engine {
                Engine::Nextflow if !PROCESS_NAME.is_match(name) => {
                    return Err(QueueAssignmentError::BadProcessName(name.clone()));
                }
                Engine::Snakemake if !is_rule_name(name) => {
                    return Err(QueueAssignmentError::BadRuleName(name.clone()));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Every queue-class the block names is one of `listed`.
    fn check_listed(&self, listed: &[String]) -> Result<(), QueueAssignmentError> {
        let mut missing: Vec<String> = Vec::new();
        for (at, queue_class) in self.entries() {
            if !listed.iter().any(|l| l == queue_class) {
                missing.push(format!("`{queue_class}` (queues.{at})"));
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(QueueAssignmentError::NotInVersion { missing, listed: listed.to_vec() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<QueueAssignment, String> {
        serde_norway::from_str(yaml).map_err(|e| e.to_string())
    }

    /// As `biffix.yaml` reads it.
    fn parse_manifest(yaml: &str) -> Result<Option<QueueAssignment>, String> {
        deserialize_manifest(serde_norway::Deserializer::from_str(yaml)).map_err(|e| e.to_string())
    }

    fn assignment(
        default: Option<&str>,
        labels: &[(&str, &str)],
        names: &[(&str, &str)],
    ) -> QueueAssignment {
        let map = |pairs: &[(&str, &str)]| {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        QueueAssignment {
            default: default.map(str::to_string),
            by_label: map(labels),
            by_name: map(names),
            unknown: BTreeMap::new(),
        }
    }

    /// Every queue-class `a` names, as the version's list, so only the selectors are judged.
    fn check(a: &QueueAssignment, engine: Engine) -> Result<(), QueueAssignmentError> {
        let listed: Vec<String> = a.entries().into_iter().map(|(_, qc)| qc.to_string()).collect();
        a.validate(engine, &listed)
    }

    #[test]
    fn the_manifest_block_parses_and_refuses_unknown_keys() {
        let yaml = "default: small\nby_label: {process_high: large}\nby_name: {ALIGN: large}\n";
        let want = assignment(Some("small"), &[("process_high", "large")], &[("ALIGN", "large")]);
        assert_eq!(parse_manifest(yaml).unwrap(), Some(want.clone()));
        assert_eq!(parse(yaml).unwrap(), want);
        assert!(parse_manifest("by_labels: {a: b}\n")
            .unwrap_err()
            .contains("unknown field `by_labels`"));
    }

    #[test]
    fn a_stored_assignment_with_a_key_added_since_still_reads() {
        // A version a newer backend stored, read by this one (as after a rollback).
        let stored = r#"{"by_name": {"align": "gpu"}, "by_resource": {"gpus": "gpu"}}"#;
        let got: QueueAssignment = serde_json::from_str(stored).unwrap();
        let mut want = assignment(None, &[], &[("align", "gpu")]);
        want.unknown.insert("by_resource".into(), serde_json::json!({"gpus": "gpu"}));
        assert_eq!(got, want);
        // The key travels with the selectors, so the coordinator can refuse what it cannot apply,
        // and keeps the assignment from reading as empty.
        assert_eq!(
            serde_json::to_value(got.selectors()).unwrap(),
            serde_json::json!({"by_name": {"align": "gpu"}, "by_resource": {"gpus": "gpu"}})
        );
        let only_unknown: QueueAssignment = serde_json::from_str(r#"{"by_x": 1}"#).unwrap();
        assert!(!only_unknown.is_empty() && !only_unknown.selectors().is_empty());
        // Registration refuses it.
        assert_eq!(
            check(&only_unknown, Engine::Nextflow),
            Err(QueueAssignmentError::UnknownKeys(vec!["by_x".into()]))
        );
    }

    #[test]
    fn the_default_queue_class_is_the_block_s_else_the_first_listed() {
        let listed = ["cpu".to_string(), "gpu".to_string()];
        assert_eq!(assignment(Some("gpu"), &[], &[]).default_queue_class(&listed), Some("gpu"));
        assert_eq!(QueueAssignment::default().default_queue_class(&listed), Some("cpu"));
        assert_eq!(QueueAssignment::default().default_queue_class(&[]), None);
    }

    #[test]
    fn labels_are_nextflow_only() {
        let a = assignment(None, &[("process_high", "large")], &[]);
        assert_eq!(check(&a, Engine::Nextflow), Ok(()));
        assert_eq!(check(&a, Engine::Snakemake), Err(QueueAssignmentError::LabelsNeedNextflow));
    }

    #[test]
    fn a_label_must_be_one_nextflow_accepts() {
        for ok in ["process_high", "a", "gpu2", "queue=large", "A_b9"] {
            assert_eq!(
                check(&assignment(None, &[(ok, "q")], &[]), Engine::Nextflow),
                Ok(()),
                "{ok}"
            );
        }
        for bad in ["", "_x", "x_", "9x", "process.*", "a b", "a=", "=b", "a=b=c", "big-one", "a\n"]
        {
            assert_eq!(
                check(&assignment(None, &[(bad, "q")], &[]), Engine::Nextflow),
                Err(QueueAssignmentError::BadLabel(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_name_is_a_nextflow_process_name_or_any_snakemake_rule_name() {
        for ok in ["ALIGN", "_x", "NFCORE_SAREK:SAREK:BWAMEM1_MEM", "ÄLIGN", "WF:çall"] {
            assert_eq!(
                check(&assignment(None, &[], &[(ok, "q")]), Engine::Nextflow),
                Ok(()),
                "{ok}"
            );
        }
        for bad in ["", "9A", "A:", ":A", "A.*", "A-B", "A\n"] {
            assert_eq!(
                check(&assignment(None, &[], &[(bad, "q")]), Engine::Nextflow),
                Err(QueueAssignmentError::BadProcessName(bad.into())),
                "{bad}"
            );
        }
        // Snakemake names a rule with any string (a `name:` directive's, or an unnamed rule's
        // number), and its rule queue map is matched exactly.
        for ok in ["align", "1", "align-bwa-mem", "a.b", "A:B", "call (gpu)"] {
            assert_eq!(
                check(&assignment(None, &[], &[(ok, "q")]), Engine::Snakemake),
                Ok(()),
                "{ok}"
            );
        }
        for bad in ["", "a\n", "a\tb", "\u{7f}"] {
            assert_eq!(
                check(&assignment(None, &[], &[(bad, "q")]), Engine::Snakemake),
                Err(QueueAssignmentError::BadRuleName(bad.into())),
                "{bad}"
            );
        }
    }

    /// `{"by_name":{"r":"<queue-class>"}}`, padded to exactly `bytes` bytes as JSON.
    fn of_size(bytes: usize) -> QueueAssignment {
        let frame = serde_json::to_string(&assignment(None, &[], &[("r", "")])).unwrap().len();
        assignment(None, &[], &[("r", &"q".repeat(bytes - frame))])
    }

    #[test]
    fn the_selectors_are_held_to_their_size() {
        let fits = of_size(MAX_QUEUE_SELECTOR_BYTES);
        assert_eq!(serde_json::to_string(&fits).unwrap().len(), 2048);
        assert_eq!(check(&fits, Engine::Snakemake), Ok(()));
        let over = of_size(MAX_QUEUE_SELECTOR_BYTES + 1);
        let err = check(&over, Engine::Snakemake).unwrap_err();
        assert_eq!(err, QueueAssignmentError::TooLarge(2049));
        assert!(err.to_string().starts_with(
            "`queues`: its selectors (the `by_label` and `by_name` entries, labels, names and \
             queue-classes) take 2049 bytes as JSON, more than the 2048 a run can carry"
        ));
        // The default does not count: it travels as the run's queue-class.
        let mut with_default = fits;
        with_default.default = Some("d".repeat(MAX_QUEUE_SELECTOR_BYTES));
        assert_eq!(check(&with_default, Engine::Snakemake), Ok(()));
    }

    #[test]
    fn every_queue_class_must_be_one_the_version_lists() {
        let a = assignment(Some("small"), &[("process_high", "large")], &[("ALIGN", "large")]);
        assert_eq!(a.validate(Engine::Nextflow, &["large".into(), "small".into()]), Ok(()));
        let err = a.validate(Engine::Nextflow, &["small".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`queues` names queue-class(es) the version does not list: `large` \
             (queues.by_label.process_high), `large` (queues.by_name.ALIGN); register with \
             `--queue-class` for each queue-class the version uses (it lists: small)"
        );
        // An empty queue-class is one no version lists.
        let empty = assignment(Some(""), &[], &[]);
        assert!(matches!(
            empty.validate(Engine::Snakemake, &["small".into()]),
            Err(QueueAssignmentError::NotInVersion { .. })
        ));
        let none = a.validate(Engine::Nextflow, &[]).unwrap_err();
        assert!(none.to_string().ends_with("(it lists none)"));
        assert_eq!(QueueAssignment::default().validate(Engine::Snakemake, &[]), Ok(()));
    }

    #[test]
    fn the_selectors_leave_out_the_default() {
        let a = assignment(Some("small"), &[("l", "large")], &[]);
        assert_eq!(a.selectors(), assignment(None, &[("l", "large")], &[]));
        assert!(!a.is_empty() && QueueAssignment::default().is_empty());
        assert_eq!(serde_json::to_string(&a.selectors()).unwrap(), r#"{"by_label":{"l":"large"}}"#);
    }
}
