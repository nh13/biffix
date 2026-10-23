//! Cross-language guard for the engine wire `SCHEMA_VERSION`.
//!
//! The version is defined three times — Rust (`wire::engine::SCHEMA_VERSION`), the Python snakemake
//! logger (`events.py`), and the Groovy nf-biffix observer (`BiffixObserver.groovy`) — and nothing
//! asserted they agree, so bumping one and not the others would silently ship a producer/consumer
//! skew. This test runs in the `rust` CI job (the whole repo is checked out) and pins the two
//! plugin definitions to the Rust source of truth by extracting each literal from its file.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // crates/biffix-core → repo root.
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("workspace root").to_path_buf()
}

/// The integer literal assigned in `<...> SCHEMA_VERSION = <int>`, which must be assigned exactly
/// once. Keys on the whole-word name followed by an assignment, on code lines only, so a comment,
/// a later use of the constant, or another constant ending in the name (`ENGINE_SCHEMA_VERSION`)
/// is ignored.
fn declared_version(source: &str) -> Option<u32> {
    let assignment = regex::Regex::new(r"\bSCHEMA_VERSION\s*=\s*(\d+)").unwrap();
    let found: Vec<u32> = source
        .lines()
        .filter(|line| {
            let code = line.trim_start();
            !["#", "//", "/*", "*"].iter().any(|c| code.starts_with(c))
        })
        .filter_map(|line| assignment.captures(line))
        .map(|c| c[1].parse().expect("a u32 literal"))
        .collect();
    match found.as_slice() {
        [version] => Some(*version),
        _ => None,
    }
}

#[test]
fn declared_version_reads_only_the_one_real_assignment() {
    let python = "# SCHEMA_VERSION = 9 (old)\nENGINE_SCHEMA_VERSION = 8\nSCHEMA_VERSION = 1\n";
    assert_eq!(declared_version(python), Some(1));
    let groovy = "    /** SCHEMA_VERSION = 9 */\n    private static final int SCHEMA_VERSION = 2\n";
    assert_eq!(declared_version(groovy), Some(2));
    assert_eq!(declared_version("x = SCHEMA_VERSION == 1\n"), None, "a use");
    assert_eq!(
        declared_version("SCHEMA_VERSION = 1\nSCHEMA_VERSION = 2\n"),
        None,
        "two assignments are ambiguous"
    );
}

#[test]
fn engine_schema_version_agrees_across_languages() {
    let root = workspace_root();
    let expected = biffix_core::wire::engine::SCHEMA_VERSION;

    let python = std::fs::read_to_string(root.join(
        "plugins/snakemake-logger-plugin-biffix/src/snakemake_logger_plugin_biffix/events.py",
    ))
    .expect("read events.py");
    assert_eq!(
        declared_version(&python),
        Some(expected),
        "python events.py SCHEMA_VERSION must match wire::engine::SCHEMA_VERSION ({expected})",
    );

    let groovy = std::fs::read_to_string(
        root.join("plugins/nf-biffix/src/main/groovy/io/github/nh13/biffix/BiffixObserver.groovy"),
    )
    .expect("read BiffixObserver.groovy");
    assert_eq!(
        declared_version(&groovy),
        Some(expected),
        "groovy BiffixObserver SCHEMA_VERSION must match wire::engine::SCHEMA_VERSION ({expected})",
    );
}
