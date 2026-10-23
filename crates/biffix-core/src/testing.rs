//! Test builders for the contract types that tests construct everywhere, so a test names only the
//! fields its case is about and a new field gets its default here once, instead of in a struct
//! literal in every test that builds one.
//!
//! Test tooling, not library API: compiled only for this crate's tests and behind the
//! `test-support` feature, which only the workspace's dev-dependencies turn on.

use crate::event::{EventData, LogSeq, RunEvent, RunSubmitted};
use crate::ids::{Attempt, BatchJobId, InstanceId, RunId, TaskKey};
use crate::source::EventSource;
use crate::time::Timestamp;

/// A normalized event at `ts` (epoch seconds) with every correlation field unset, as the
/// production constructors start from. Set the fields a case needs with [`RunEventExt`].
pub fn event(
    ts: f64,
    source: EventSource,
    dedup_key: impl Into<String>,
    data: EventData,
) -> RunEvent {
    RunEvent::new(dedup_key.into(), Timestamp::from_epoch_seconds(ts), source, data)
}

/// A `run_submitted` anchor with placeholder required fields (version `wf`, sha `abc`, image
/// `sha256:x`, no params or inputs) and every optional field unset. Override what a case checks
/// with struct-update syntax: `RunSubmitted { run_name: Some(..), ..anchor() }`.
pub fn anchor() -> RunSubmitted {
    RunSubmitted::new("wf", "abc", "sha256:x", serde_json::json!({}), vec![])
}

/// Builder-style setters for a test event's correlation fields:
/// `event(..).with_run("r1").with_task("align").with_attempt(1).with_job("j1")`.
pub trait RunEventExt: Sized {
    /// Attributed to run `run`.
    fn with_run(self, run: &str) -> Self;
    /// Keyed on task `key`.
    fn with_task(self, key: &str) -> Self;
    /// Attempt number `n`.
    fn with_attempt(self, n: u32) -> Self;
    /// About Batch job `job`.
    fn with_job(self, job: &str) -> Self;
    /// Reported from the (container-instance) host `host`.
    fn with_instance(self, host: &str) -> Self;
    /// Run on the Batch queue `queue`.
    fn with_queue(self, queue: &str) -> Self;
    /// At log position `seq`.
    fn with_seq(self, seq: i64) -> Self;
}

impl RunEventExt for RunEvent {
    fn with_run(mut self, run: &str) -> Self {
        self.run_id = Some(RunId::new(run));
        self
    }

    fn with_task(mut self, key: &str) -> Self {
        self.task_key = Some(TaskKey::new(key));
        self
    }

    fn with_attempt(mut self, n: u32) -> Self {
        self.attempt = Some(Attempt(n));
        self
    }

    fn with_job(mut self, job: &str) -> Self {
        self.batch_job_id = Some(BatchJobId::new(job));
        self
    }

    fn with_instance(mut self, host: &str) -> Self {
        self.instance_id = Some(InstanceId::new(host));
        self
    }

    fn with_queue(mut self, queue: &str) -> Self {
        self.queue = Some(queue.into());
        self
    }

    fn with_seq(mut self, seq: i64) -> Self {
        self.seq = Some(LogSeq(seq));
        self
    }
}

/// The environment the deploy's CDK (Python) sets on a Lambda, read as text, for the tests that
/// hold each binary's config to it: a binary must read every variable its Lambda is given, and its
/// Lambda must be given every variable the binary requires.
pub mod cdk {
    use std::collections::BTreeSet;

    /// The variable names `python` sets: each quoted `"BIFFIX_…"` literal, and each f-string name
    /// `f"BIFFIX_…_{…}"` as the pattern `BIFFIX_…_*`.
    pub fn env_names(python: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for (at, _) in python.match_indices("\"BIFFIX_") {
            let rest = &python[at + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(rest.len());
            match rest[end..].chars().next() {
                Some('"') => names.insert(rest[..end].to_owned()),
                Some('{') if python[..at].ends_with('f') => {
                    names.insert(format!("{}*", &rest[..end]))
                }
                _ => false,
            };
        }
        names
    }

    /// The block of `python` that starts at the first line containing `start`: that line and the
    /// lines after it indented deeper (a `def`'s body, a multi-line argument).
    pub fn block<'a>(python: &'a str, start: &str) -> &'a str {
        let at = python.find(start).unwrap_or_else(|| panic!("no {start:?} in the CDK source"));
        let line_start = python[..at].rfind('\n').map_or(0, |i| i + 1);
        let indent = |line: &str| line.len() - line.trim_start().len();
        let depth = indent(&python[line_start..]);
        let mut end = python.len();
        let mut offset = line_start;
        for (i, line) in python[line_start..].split_inclusive('\n').enumerate() {
            if i > 0 && !line.trim().is_empty() && indent(line) <= depth {
                end = offset;
                break;
            }
            offset += line.len();
        }
        &python[line_start..end]
    }

    /// Assert the contract between a Lambda's CDK environment `set` and the variables its binary
    /// `read`: every one set is read (a pattern `X*` by some name starting with `X`), and of each
    /// group in `required` (one name, or names one of which suffices) one is set.
    pub fn assert_contract(
        lambda: &str,
        set: &BTreeSet<String>,
        read: &BTreeSet<String>,
        required: &[Vec<String>],
    ) {
        for name in set {
            let is_read = match name.strip_suffix('*') {
                Some(prefix) => read.iter().any(|r| r.starts_with(prefix)),
                None => read.contains(name),
            };
            assert!(
                is_read,
                "the CDK sets {name} on the {lambda} Lambda, but its binary never reads it"
            );
        }
        for group in required {
            assert!(
                group.iter().any(|name| set.contains(name)),
                "the {lambda} Lambda's binary requires {}, but the CDK sets none of it",
                group.join(" or ")
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const PY: &str = r#"
class X:
    def a(self, fn):
        fn.add_environment("BIFFIX_A", "1")
        names = {f"BIFFIX_B_{arch.upper()}": 1, "NOT_OURS": 2}
        """`BIFFIX_DOC` in a docstring."""

    def b(self, fn):
        fn.add_environment("BIFFIX_C", "1")
"#;

        #[test]
        fn names_and_blocks_are_read_from_the_python() {
            assert_eq!(
                env_names(PY),
                ["BIFFIX_A", "BIFFIX_B_*", "BIFFIX_C"].map(String::from).into()
            );
            assert_eq!(
                env_names(block(PY, "def a(")),
                ["BIFFIX_A", "BIFFIX_B_*"].map(String::from).into()
            );
            let read: BTreeSet<String> = ["BIFFIX_A", "BIFFIX_B_AMD64"].map(String::from).into();
            let required = [vec!["BIFFIX_A".to_owned()]];
            assert_contract("x", &env_names(block(PY, "def a(")), &read, &required);
        }

        #[test]
        #[should_panic(expected = "never reads it")]
        fn a_variable_set_but_never_read_breaks_the_contract() {
            assert_contract("x", &env_names(PY), &BTreeSet::new(), &[]);
        }

        #[test]
        #[should_panic(expected = "the CDK sets none of it")]
        fn a_required_variable_the_cdk_never_sets_breaks_the_contract() {
            let required = [vec!["BIFFIX_D".to_owned()]];
            assert_contract("x", &BTreeSet::new(), &BTreeSet::new(), &required);
        }
    }
}
