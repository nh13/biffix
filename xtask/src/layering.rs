//! The workspace's crate layering, checked by `cargo test` against `cargo metadata`:
//!
//! - `biffix-core` (the RunEvent contract, its folds and the small infra helpers) depends on no
//!   other workspace crate, so every crate can build on it;
//! - `biffix-workflow` (workflow authoring and registration) builds on `biffix-core` alone;
//! - the ingest path (`biffix-ingest`, `biffix-db`) never links `biffix-workflow`.

use std::collections::{BTreeMap, BTreeSet};

/// Each workspace crate's dependencies on other workspace crates, by dependency kind: `normal`,
/// `dev` or `build`.
type Graph = BTreeMap<String, Vec<(String, String)>>;

/// The workspace's graph, read once for all the tests.
static GRAPH: std::sync::LazyLock<Graph> = std::sync::LazyLock::new(workspace_graph);

fn workspace_graph() -> Graph {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
    // `--no-deps`: the workspace's own packages only, read from the manifests (no resolution, no
    // network). A workspace crate can reach another only through workspace crates, so this is the
    // whole internal graph.
    let out = std::process::Command::new(option_env!("CARGO").unwrap_or("cargo"))
        .args(["metadata", "--no-deps", "--format-version", "1", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
    let packages = meta["packages"].as_array().expect("packages");
    let members: BTreeSet<&str> =
        packages.iter().map(|p| p["name"].as_str().expect("package name")).collect();
    packages
        .iter()
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .filter_map(|d| {
                    let name = d["name"].as_str().expect("dependency name");
                    let kind = d["kind"].as_str().unwrap_or("normal");
                    members.contains(name).then(|| (name.to_string(), kind.to_string()))
                })
                .collect();
            (p["name"].as_str().expect("package name").to_string(), deps)
        })
        .collect()
}

/// The workspace crates `from` links in a production build: its normal and build dependencies,
/// transitively (optional ones included).
fn linked(graph: &Graph, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![from.to_string()];
    while let Some(name) = todo.pop() {
        for (dep, kind) in graph.get(&name).map(Vec::as_slice).unwrap_or_default() {
            if kind != "dev" && seen.insert(dep.clone()) {
                todo.push(dep.clone());
            }
        }
    }
    seen
}

#[test]
fn core_depends_on_no_other_workspace_crate() {
    let graph = &*GRAPH;
    let others: Vec<_> =
        graph["biffix-core"].iter().filter(|(name, _)| name != "biffix-core").collect();
    assert!(others.is_empty(), "biffix-core depends on {others:?}");
}

#[test]
fn workflow_builds_on_core_alone() {
    let graph = &*GRAPH;
    let deps: BTreeSet<_> =
        graph["biffix-workflow"].iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(deps, BTreeSet::from(["biffix-core"]));
}

#[test]
fn ingest_path_does_not_link_workflow() {
    let graph = &*GRAPH;
    for krate in ["biffix-ingest", "biffix-db"] {
        let deps = linked(graph, krate);
        assert!(deps.contains("biffix-core"), "{krate}: {deps:?}");
        assert!(!deps.contains("biffix-workflow"), "{krate} links biffix-workflow: {deps:?}");
    }
}

#[test]
fn linked_follows_normal_and_build_edges_but_not_dev() {
    let edge = |n: &str, k: &str| (n.to_string(), k.to_string());
    let graph = Graph::from([
        ("a".into(), vec![edge("b", "normal"), edge("t", "dev")]),
        ("b".into(), vec![edge("c", "build")]),
        ("t".into(), vec![edge("x", "normal")]),
    ]);
    assert_eq!(linked(&graph, "a"), BTreeSet::from(["b".into(), "c".into()]));
}
