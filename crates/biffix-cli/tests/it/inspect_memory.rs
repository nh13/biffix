//! `biffix inspect` on a Snakemake checkout warns (stderr, exit 0) when rules would run on AWS Batch
//! with Snakemake's default memory, and says nothing once the workflow profile sets a default.

use std::path::Path;
use std::process::Output;

use crate::common::{checkout, succeeded};

/// `biffix inspect <root>` (as JSON with `json`), with `root` as its home.
fn inspect(root: &Path, json: bool) -> Output {
    let mut cmd = crate::common::biffix(root);
    if json {
        cmd.args(["--output", "json"]);
    }
    cmd.arg("inspect").arg(root).output().unwrap()
}

const SNAKEFILE: &str = "rule all:\n    input: \"a.txt\"\n\nrule make:\n    output: \"a.txt\"\n    shell: \"touch {output}\"\n";
const MANIFEST: &str =
    "engine: snakemake\nengine_version: \"9.27.0\"\nentrypoint: workflow/Snakefile\n";

/// Whether stderr carries a `[biffix] warning:` line.
fn warns(stderr: &str) -> bool {
    stderr.lines().any(|l| l.starts_with("[biffix] warning:"))
}

#[test]
fn inspect_warns_about_rules_without_memory() {
    let dir = checkout(&[("biffix.yaml", MANIFEST), ("workflow/Snakefile", SNAKEFILE)]);
    for json in [false, true] {
        let out = inspect(dir.path(), json);
        let stderr = succeeded(&out);
        assert!(
            stderr.contains(
                "[biffix] warning: 1 rule(s) set no memory, and the workflow profile sets no \
                 default-resources memory: workflow/Snakefile:4 make. On AWS Batch each gets \
                 Snakemake's default mem_mb, min(max(2×input, 1000), 8000) MB, and the AWS Batch \
                 executor will not submit a job under 1024 MB, so one whose inputs total under \
                 512 MB fails to submit. Set default-resources: [mem_mb=…] in \
                 workflow/profiles/default/config.yaml (or resources: mem_mb=… per rule).\n"
            ),
            "{stderr}"
        );
        // The JSON document on stdout is unchanged.
        if json {
            let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(doc["engine"], "snakemake");
        }
    }
}

#[test]
fn inspect_is_quiet_when_the_workflow_profile_sets_memory() {
    let dir = checkout(&[
        ("biffix.yaml", MANIFEST),
        ("workflow/Snakefile", SNAKEFILE),
        ("workflow/profiles/default/config.yaml", "default-resources:\n  - mem_mb=30000\n"),
    ]);
    let out = inspect(dir.path(), false);
    let stderr = succeeded(&out);
    assert!(!warns(&stderr), "{stderr}");
}

/// With no entrypoint, inspect checks `Snakefile` as `register` records it (and the run uses), not
/// a `workflow/Snakefile` the run would never start from.
#[test]
fn inspect_checks_the_entrypoint_register_would_record() {
    let dir = checkout(&[
        ("biffix.yaml", "engine: snakemake\nengine_version: \"9.27.0\"\n"),
        ("Snakefile", "include: \"workflow/rules/a.smk\"\n"),
        ("workflow/rules/a.smk", SNAKEFILE),
        ("workflow/Snakefile", "rule other:\n    output: \"o\"\n    shell: \"x\"\n"),
        ("workflow/profiles/default/config.yaml", "default-resources:\n  - mem_mb=30000\n"),
    ]);
    let out = inspect(dir.path(), false);
    let stderr = succeeded(&out);
    assert!(warns(&stderr), "{stderr}");
    assert!(stderr.contains(": workflow/rules/a.smk:4 make. "), "{stderr}");
    assert!(stderr.contains(" in profiles/default/config.yaml "), "{stderr}");
}

/// With no top-level Snakefile and no `entrypoint`, inspect checks the Snakefile Snakemake would
/// run, says so, and names only biffix.yaml for pinning it (inspect has no --entrypoint).
#[test]
fn inspect_finds_the_snakefile_where_snakemake_looks() {
    let dir = checkout(&[
        ("biffix.yaml", "engine: snakemake\nengine_version: \"9.27.0\"\n"),
        ("workflow/Snakefile", SNAKEFILE),
    ]);
    let out = inspect(dir.path(), false);
    let stderr = succeeded(&out);
    assert!(
        stderr.contains("[biffix] note: no top-level Snakefile; using workflow/Snakefile"),
        "{stderr}"
    );
    assert!(!stderr.contains("--entrypoint"), "{stderr}");
    assert!(
        stderr.contains(": workflow/Snakefile:4 make. "),
        "the memory check still runs: {stderr}"
    );
}

/// An entrypoint `register` would refuse is only a warning: the images are still reported, and
/// the warning names biffix.yaml, not a flag inspect does not have.
#[test]
fn inspect_warns_about_an_entrypoint_register_would_refuse() {
    let dir = checkout(&[
        ("biffix.yaml", "engine: snakemake\nengine_version: \"9.27.0\"\nentrypoint: Snakefile\n"),
        ("workflow/Snakefile", SNAKEFILE),
    ]);
    let out = inspect(dir.path(), false);
    let stderr = succeeded(&out);
    assert!(
        stderr.contains(
            "[biffix] warning: entrypoint `Snakefile` (from biffix.yaml `entrypoint`) is not in \
             the source biffix ships"
        ) && stderr.contains("did you mean `workflow/Snakefile`?"),
        "{stderr}"
    );
    assert!(!stderr.contains("--entrypoint"), "{stderr}");
    assert!(stderr.contains("[biffix] found 0 images"), "{stderr}");
    assert!(!stderr.contains("set no memory"), "{stderr}");
}
