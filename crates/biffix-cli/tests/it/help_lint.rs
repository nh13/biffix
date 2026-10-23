//! `biffix inspect` and `biffix workflows form` warn (never fail) when the launch form shows a
//! param with no description or help, and say nothing once each one explains itself. `register`
//! prints the same warning from the same function before it stages anything.

use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

use crate::common::{checkout, succeeded};

/// `biffix <args> <root>`, with `root` as its home.
fn biffix(root: &Path, args: &[&str]) -> Output {
    crate::common::biffix(root).args(args).arg(root).output().unwrap()
}

/// A Snakemake rule that sets its memory, so the memory advisory stays quiet.
const SNAKEFILE: &str = "rule all:\n    input: \"a.txt\"\n\nrule make:\n    output: \"a.txt\"\n    resources: mem_mb=2000\n    shell: \"touch {output}\"\n";

/// A config schema: `samples` (required) says nothing; `aligner` is described; `extra` is
/// optional, so it sits under the advanced *Other options* and is not linted (unless a `form:`
/// overlay puts it in a shown section).
const SCHEMA: &str = "properties:\n  samples: {type: string}\n  aligner: {type: string, description: Which aligner}\n  extra: {type: string}\nrequired: [samples, aligner]\n";

fn manifest(params: &str) -> String {
    format!("engine: snakemake\nengine_version: \"9.27.0\"\nschema: config/schema.yaml\n{params}")
}

/// A checkout of the [`manifest`] with `params` appended, [`SNAKEFILE`], and `schema`.
fn form_checkout(params: &str, schema: &str) -> TempDir {
    checkout(&[
        ("biffix.yaml", &manifest(params)),
        ("Snakefile", SNAKEFILE),
        ("config/schema.yaml", schema),
    ])
}

const LINT: &str = "[biffix] warning: 1 parameter(s) the launch form shows have no description \
                    or help: samples. A biologist sees only the name; add a `description` (or \
                    `help_text`) in the schema, or `help:` under `params.<name>` in biffix.yaml\n";

#[test]
fn inspect_and_form_warn_about_a_param_with_nothing_to_explain_it() {
    let dir = form_checkout("", SCHEMA);
    let out = biffix(dir.path(), &["inspect"]);
    let stderr = succeeded(&out);
    assert!(stderr.contains(LINT), "{stderr}");

    // `workflows form` lists it among the form's warnings.
    let out = biffix(dir.path(), &["--output", "human", "workflows", "form"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        stdout.contains("Warnings:\n  - 1 parameter(s) the launch form shows have no description or help: samples."),
        "{stdout}"
    );
    // As JSON (what a CI check reads), the form is on stdout and the lint on stderr, as
    // `register` prints it.
    let out = biffix(dir.path(), &["--output", "json", "workflows", "form"]);
    let stderr = succeeded(&out);
    assert!(stderr.contains(LINT), "{stderr}");
    serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("the form as JSON");
}

#[test]
fn a_biffix_yaml_help_hint_quiets_the_warning() {
    let dir = form_checkout(
        "params:\n  samples:\n    help: The sample table, one row per sample.\n",
        SCHEMA,
    );
    let out = biffix(dir.path(), &["inspect"]);
    let stderr = succeeded(&out);
    // No warning at all, whatever its wording.
    assert!(!stderr.contains("[biffix] warning: "), "{stderr}");
    let out = biffix(dir.path(), &["--output", "human", "workflows", "form"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("No warnings."), "{stdout}");
}

#[test]
fn an_optional_param_in_a_shown_section_is_linted_too() {
    let dir = form_checkout("form:\n  sections:\n    - { id: main, title: Main }\n  params:\n    extra: { section: main }\n", SCHEMA);
    let out = biffix(dir.path(), &["inspect"]);
    let stderr = succeeded(&out);
    assert!(
        stderr.contains(
            "[biffix] warning: 2 parameter(s) the launch form shows have no description or help: "
        ),
        "{stderr}"
    );
    assert!(stderr.contains("extra"), "{stderr}");
}

#[test]
fn inspect_still_reports_when_the_form_cannot_be_read() {
    let dir = form_checkout("", "properties: [not, a, map");
    let out = biffix(dir.path(), &["inspect"]);
    let stderr = succeeded(&out);
    assert!(stderr.contains("[biffix] warning: cannot read the launch form: "), "{stderr}");
}
