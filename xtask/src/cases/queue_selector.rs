//! A table of `queues:` selectors (labels and names) with whether each engine accepts them,
//! shared across languages.
//!
//! Registration checks a version's `queues:` block ([`QueueAssignment::validate`]), and the
//! coordinator checks it again before writing it into the engine's config
//! (`docker/coordinator/lib/queue_selectors.py`), with its own Python copy of the grammars: a
//! Nextflow selector that is not an exact name would match other tasks, so the copies must agree.
//! These cases are asserted against the Rust validation (`tests` below) and emitted to
//! `schemas/queue-selector-cases.json` (by `cargo xtask emit-schemas`), which the coordinator's
//! tests replay against its copy.

use serde::Serialize;

#[cfg(test)]
use biffix_workflow::{engines::Engine, queues::QueueAssignment};

#[derive(Debug, Serialize)]
pub(crate) struct QueueSelectorCase {
    /// `nextflow` or `snakemake`.
    engine: &'static str,
    /// `by_label` or `by_name`.
    key: &'static str,
    /// The label or name.
    selector: String,
    /// Whether the engine's selectors accept it.
    accepted: bool,
}

fn case(
    engine: &'static str,
    key: &'static str,
    selector: &str,
    accepted: bool,
) -> QueueSelectorCase {
    QueueSelectorCase { engine, key, selector: selector.into(), accepted }
}

/// The shared cases.
pub(crate) fn cases() -> Vec<QueueSelectorCase> {
    let mut cases = Vec::new();
    // Nextflow labels, as its `label` directive accepts them (ProcessBuilder.isValidLabel).
    for ok in ["process_high", "a", "gpu2", "queue=large", "A_b9"] {
        cases.push(case("nextflow", "by_label", ok, true));
    }
    for bad in
        ["", "_x", "x_", "9x", "process.*", "a b", "a=", "=b", "a=b=c", "big-one", "a\n", "ä"]
    {
        cases.push(case("nextflow", "by_label", bad, false));
    }
    // Nextflow process names: identifiers (Unicode letters included), or `:`-joined ones.
    // `A\u{301}` is a decomposed `Á`: a combining mark continues an identifier.
    for ok in
        ["ALIGN", "_x", "NFCORE_SAREK:SAREK:BWAMEM1_MEM", "ÄLIGN", "WF:çall", "a1_B2", "A\u{301}"]
    {
        cases.push(case("nextflow", "by_name", ok, true));
    }
    for bad in ["", "9A", "A:", ":A", "A::B", "A.*", "A|B", "A-B", "A B", "A\n", "A'", "A$"] {
        cases.push(case("nextflow", "by_name", bad, false));
    }
    // Snakemake rule names: any non-empty name with no control character.
    for ok in ["align", "1", "align-bwa-mem", "a.b", "A:B", "call (gpu)", "ālign", "a'b"] {
        cases.push(case("snakemake", "by_name", ok, true));
    }
    for bad in ["", "a\n", "a\tb", "\u{7f}", "a\u{85}", "\u{9f}"] {
        cases.push(case("snakemake", "by_name", bad, false));
    }
    // Snakemake rules have no labels.
    cases.push(case("snakemake", "by_label", "process_high", false));
    cases
}

pub(crate) fn cases_json() -> String {
    crate::artifacts::pretty_json(&cases())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_holds_for_the_rust_validation() {
        for c in cases() {
            let engine = match c.engine {
                "nextflow" => Engine::Nextflow,
                _ => Engine::Snakemake,
            };
            let entry = [(c.selector.clone(), "q".to_string())].into();
            let queues = match c.key {
                "by_label" => QueueAssignment { by_label: entry, ..Default::default() },
                _ => QueueAssignment { by_name: entry, ..Default::default() },
            };
            assert_eq!(queues.validate(engine, &["q".into()]).is_ok(), c.accepted, "{c:?}");
        }
    }
}
