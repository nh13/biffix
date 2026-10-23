//! A table of image references with their normalized spellings, shared across languages.
//!
//! A run's image map is keyed by [`ImageRef`]'s normalized spelling of each declared reference
//! (`crates/biffix-backend/src/images/map.rs`), and nf-biffix looks a task's container up in it
//! through a JVM port of the same normalization (`ImageMap.normalize`); a drift between the two
//! fails a task at run time as an image biffix "did not import". To catch it at review time
//! instead, these cases are asserted against [`ImageRef::parse`] (`tests` below) and emitted to
//! `schemas/image-ref-cases.json` (by `cargo xtask emit-schemas`), which the plugin's tests replay
//! against its port. A case the Rust parser rejects has no normalized spelling; the port does not
//! validate, so it skips those.

use serde::Serialize;

#[cfg(test)]
use biffix_workflow::images::ImageRef;

#[derive(Debug, Serialize)]
pub(crate) struct ImageRefCase {
    /// The reference as a workflow writes it.
    input: String,
    /// Its normalized spelling ([`ImageRef`]'s `Display`), or `None` when it is rejected.
    normalized: Option<String>,
}

const D_LOWER: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const D_UPPER: &str = "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn case(input: impl Into<String>, normalized: Option<String>) -> ImageRefCase {
    ImageRefCase { input: input.into(), normalized }
}

fn ok(input: impl Into<String>, normalized: impl Into<String>) -> ImageRefCase {
    case(input, Some(normalized.into()))
}

/// The shared cases.
pub(crate) fn cases() -> Vec<ImageRefCase> {
    let ecr = "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workers";
    vec![
        // Docker Hub: a one-part name is under `library/`, no tag means `latest`.
        ok("ubuntu", "docker.io/library/ubuntu:latest"),
        ok("ubuntu:24.04", "docker.io/library/ubuntu:24.04"),
        ok("  ubuntu:24.04  ", "docker.io/library/ubuntu:24.04"),
        ok(
            "biocontainers/fastqc:0.12.1--hdfd78af_0",
            "docker.io/biocontainers/fastqc:0.12.1--hdfd78af_0",
        ),
        ok("docker.io/ubuntu", "docker.io/library/ubuntu:latest"),
        ok("index.docker.io/ubuntu", "docker.io/library/ubuntu:latest"),
        ok("registry-1.docker.io/library/ubuntu:24.04", "docker.io/library/ubuntu:24.04"),
        ok("x:_tag", "docker.io/library/x:_tag"),
        // A registry: the first component has a `.` or `:` or is `localhost`; it is lowercased.
        ok(
            "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0",
            "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0",
        ),
        ok("Quay.IO/biocontainers/fastqc:1", "quay.io/biocontainers/fastqc:1"),
        ok("Index.Docker.io/ubuntu", "docker.io/library/ubuntu:latest"),
        ok("localhost:5000/coord", "localhost:5000/coord:latest"),
        ok("localhost/coord:1", "localhost/coord:1"),
        ok("[::1]:5000/x:1", "[::1]:5000/x:1"),
        ok("my-registry.local:5000/team/tool", "my-registry.local:5000/team/tool:latest"),
        // A digest: lowercased; with no tag, no `latest` is added.
        ok(format!("quay.io/b/bwa@{D_LOWER}"), format!("quay.io/b/bwa@{D_LOWER}")),
        ok(format!("quay.io/x/y:1@{D_UPPER}"), format!("quay.io/x/y:1@{D_LOWER}")),
        ok(format!("ghcr.io/org/tool:1.0@{D_LOWER}"), format!("ghcr.io/org/tool:1.0@{D_LOWER}")),
        ok(format!("{ecr}@{D_LOWER}"), format!("{ecr}@{D_LOWER}")),
        // Rejected by biffix-workflow (the port does not validate, so these have no shared
        // spelling).
        case("", None),
        case("UPPER/repo", None),
        case("https://quay.io/x/y", None),
        case("quay.io/x/y@sha256:short", None),
        case("quay.io/x/y:bad tag", None),
        case(format!("@{D_LOWER}"), None),
        // Too long once normalized (`docker.io/library/` + 240), though not as typed.
        case("a".repeat(240), None),
    ]
}

/// The cases as the JSON the plugin replays.
pub(crate) fn cases_json() -> String {
    crate::artifacts::pretty_json(&cases())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_holds_for_the_rust_parser() {
        for case in cases() {
            let got = ImageRef::parse(&case.input).ok().map(|r| r.to_string());
            assert_eq!(got, case.normalized, "{:?}", case.input);
        }
    }

    #[test]
    fn a_normalized_spelling_is_its_own_normal_form() {
        for case in cases() {
            if let Some(normalized) = &case.normalized {
                assert_eq!(
                    ImageRef::parse(normalized).map(|r| r.to_string()).as_ref(),
                    Ok(normalized),
                    "{normalized}"
                );
            }
        }
    }
}
