//! The engine versions biffix supports: a workflow version pins its engine to one exact release
//! (`engine_version: "9.3.0"` in `biffix.yaml`), which must fall in that engine's supported range.
//!
//! Shared by the CLI (fails fast, before staging anything) and the backend (the authority: rejects
//! a registration with 400). The range is what biffix can build an engine runtime for; widening it
//! is a data change here.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A workflow engine biffix runs. The one definition of the engine ↔ string mapping: the backend
/// domain (a workflow version's `engine`, its DB column, the coordinator's `BIFFIX_ENGINE`), the
/// CLI and the manifest's [`crate::manifest::EngineChoice`] all go through [`Engine::as_str`] and
/// its [`std::str::FromStr`] inverse, whose strings are the serde (`snake_case`) form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Snakemake,
    Nextflow,
}

impl Engine {
    /// Every engine, in declaration order.
    pub const ALL: [Engine; 2] = [Engine::Snakemake, Engine::Nextflow];

    /// The canonical lowercase name, identical to the serde form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Engine::Snakemake => "snakemake",
            Engine::Nextflow => "nextflow",
        }
    }

    /// Why `target` cannot be this engine's target (`None` when it can). The coordinator passes a
    /// target as one word of the engine's command line (Snakemake: a positional target after its
    /// own options; Nextflow: `-entry <target>`), so it must read as a target and never as an
    /// option or more than one word:
    ///
    /// - Snakemake: a rule name or a file path relative to the workflow, made of ASCII letters,
    ///   digits and `_ . - + /`, starting with a letter, digit or `_` (never `-` or `/`);
    /// - Nextflow: a workflow name, an identifier (`[A-Za-z_][A-Za-z0-9_]*`).
    ///
    /// At most [`MAX_TARGET_CHARS`] characters.
    pub fn target_problem(self, target: &str) -> Option<String> {
        let first_ok = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let (ok, what) = match self {
            Engine::Snakemake => (
                target.starts_with(first_ok)
                    && target.chars().all(|c| first_ok(c) || matches!(c, '.' | '-' | '+' | '/')),
                "a rule name or a relative file path (ASCII letters, digits and `_ . - + /`, \
                 starting with a letter, digit or `_`)",
            ),
            Engine::Nextflow => (
                target.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && target.chars().all(first_ok),
                "a workflow name (ASCII letters, digits and `_`, not starting with a digit)",
            ),
        };
        if target.chars().count() > MAX_TARGET_CHARS {
            return Some(format!("a {self} target is at most {MAX_TARGET_CHARS} characters"));
        }
        (!ok).then(|| format!("a {self} target must be {what}; got {target:?}"))
    }
}

/// The longest target ([`Engine::target_problem`]) accepted.
pub const MAX_TARGET_CHARS: usize = 256;

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The parse error for an unrecognized engine name.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsupported engine {0:?} (supported: snakemake, nextflow)")]
pub struct UnknownEngine(pub String);

impl std::str::FromStr for Engine {
    type Err = UnknownEngine;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Engine::ALL.into_iter().find(|e| e.as_str() == s).ok_or_else(|| UnknownEngine(s.to_owned()))
    }
}

/// An exact, stable engine release: `MAJOR.MINOR.PATCH`, digits only (no `-edge`, no ranges).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EngineVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl EngineVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// Parse an exact `MAJOR.MINOR.PATCH`; `None` for anything else (`9`, `9.3`, `>=9`, `25.10.0-edge`).
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.split('.');
        let mut next = || {
            parts
                .next()
                .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|p| p.parse().ok())
        };
        let version = Self::new(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for EngineVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One engine's supported releases: `min` (inclusive) up to `below` (exclusive), each written the
/// way the engine writes its versions (Nextflow zero-pads the month: `25.04.0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportedRange {
    pub engine: &'static str,
    pub min: &'static str,
    pub below: &'static str,
    /// The only releases in the range biffix can build, when that is a fixed list (empty: any
    /// release in the range). Nextflow's are the releases whose launcher and plugin downloads the
    /// runtime pins by sha256 ([`crate::runtime`]); an unpinned one could never build.
    pub releases: &'static [&'static str],
}

impl SupportedRange {
    pub fn contains(&self, version: EngineVersion) -> bool {
        let bound = |s| EngineVersion::parse(s).expect("SUPPORTED bounds are exact versions");
        bound(self.min) <= version && version < bound(self.below)
    }
}

impl fmt::Display for SupportedRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ">={}, <{}", self.min, self.below)
    }
}

/// The Nextflow releases biffix can build: each has its launcher and plugins pinned by sha256 in
/// the runtime's pin table (a test keeps the two in step). Spelled as Nextflow writes them.
pub(crate) const NEXTFLOW_RELEASES: &[&str] = &["25.10.4"];

/// A release biffix once built and no longer does: a registration pinning it, and a launch of a
/// version registered on it, are refused by name with the release to re-register on, rather than
/// failing later as an unbuildable or out-of-range release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retired {
    pub engine: Engine,
    /// Spelled as the engine writes it.
    pub release: &'static str,
    /// The supported release to re-register on.
    pub replacement: &'static str,
}

impl fmt::Display for Retired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self.engine {
            Engine::Snakemake => "Snakemake",
            Engine::Nextflow => "Nextflow",
        };
        write!(
            f,
            "{label} {} is no longer supported; re-register on {}",
            self.release, self.replacement
        )
    }
}

/// The retired releases. Nextflow 25.04.0 (nf-amazon 2.x, AWS SDK v1): nf-biffix builds against
/// Nextflow 25.10 and nf-amazon 3.x only.
pub const RETIRED: &[Retired] =
    &[Retired { engine: Engine::Nextflow, release: "25.04.0", replacement: "25.10.4" }];

/// The retirement of `engine` at `version` (any spelling, e.g. Nextflow's `25.4.0`), if retired.
pub fn retired(engine: &str, version: &str) -> Option<Retired> {
    let parsed = EngineVersion::parse(version.trim())?;
    let canonical = spelling(engine, parsed);
    RETIRED.iter().copied().find(|r| r.engine.as_str() == engine && r.release == canonical)
}

/// Every engine biffix runs and its supported range.
///
/// - Snakemake 9: the biffix logger plugin and the S3 storage plugin line the coordinator uses both
///   require it.
/// - Nextflow from 25.10.4: the nf-biffix plugin's declared minimum (it builds against Nextflow
///   25.10 and nf-amazon 3.x).
pub const SUPPORTED: &[SupportedRange] = &[
    SupportedRange { engine: "snakemake", min: "9.0.0", below: "10.0.0", releases: &[] },
    SupportedRange {
        engine: "nextflow",
        min: "25.10.4",
        below: "26.0.0",
        releases: NEXTFLOW_RELEASES,
    },
];

/// The supported range for `engine` (`snakemake` | `nextflow`), or `None` for an unknown engine.
pub(crate) fn supported_range(engine: &str) -> Option<SupportedRange> {
    SUPPORTED.iter().copied().find(|r| r.engine == engine)
}

/// Why a pinned engine version is not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EngineVersionError {
    #[error("unknown engine `{0}`")]
    UnknownEngine(String),
    /// The example pin is the range's minimum.
    #[error(
        "{engine} engine_version `{given}` is not an exact release; pin one like `{}` \
         (supported: {range})",
        .range.min
    )]
    NotExact { engine: String, given: String, range: SupportedRange },
    /// In the range, but not one of the releases biffix can build.
    #[error(
        "{engine} {given} is not a release biffix can build; pin one of {}",
        .releases.join(", ")
    )]
    NotBuildable { engine: String, given: String, releases: &'static [&'static str] },
    /// A release biffix no longer builds, named with the release to re-register on.
    #[error("{0}")]
    Retired(Retired),
    #[error("{engine} {given} is not supported; biffix supports {engine} {range}")]
    Unsupported { engine: String, given: String, range: SupportedRange },
}

/// An engine release as a workflow version pins it: the parsed version and its canonical spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedVersion {
    pub version: EngineVersion,
    /// The one spelling biffix records and builds with ([`spelling`]): `9.03.0` is `9.3.0`, and
    /// Nextflow's `25.4.0` is `25.04.0`, so equal releases are equal strings.
    pub canonical: String,
}

/// How `engine` writes `version`: Nextflow zero-pads the month (`25.04.0`, which the build passes
/// straight to `NXF_VER`); every other engine writes plain numbers (`9.3.0`).
pub fn spelling(engine: &str, version: EngineVersion) -> String {
    match engine {
        "nextflow" => format!("{}.{:02}.{}", version.major, version.minor, version.patch),
        _ => version.to_string(),
    }
}

/// Check that `version` is an exact release of `engine` inside its supported range, and spell it
/// canonically.
pub fn check_engine_version(
    engine: &str,
    version: &str,
) -> Result<PinnedVersion, EngineVersionError> {
    let range =
        supported_range(engine).ok_or_else(|| EngineVersionError::UnknownEngine(engine.into()))?;
    let given = version.trim();
    let parsed = EngineVersion::parse(given).ok_or_else(|| EngineVersionError::NotExact {
        engine: engine.into(),
        given: given.into(),
        range,
    })?;
    if let Some(r) = retired(engine, version) {
        return Err(EngineVersionError::Retired(r));
    }
    if !range.contains(parsed) {
        return Err(EngineVersionError::Unsupported {
            engine: engine.into(),
            given: given.into(),
            range,
        });
    }
    let canonical = spelling(engine, parsed);
    if !range.releases.is_empty() && !range.releases.contains(&canonical.as_str()) {
        return Err(EngineVersionError::NotBuildable {
            engine: engine.into(),
            given: given.into(),
            releases: range.releases,
        });
    }
    Ok(PinnedVersion { version: parsed, canonical })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_reads_as_one_target_never_an_option() {
        for ok in ["all", "results/S1.bam", "qc_report", "out/a+b.txt", "_private"] {
            assert_eq!(Engine::Snakemake.target_problem(ok), None, "{ok}");
        }
        for bad in [
            "--retries=1000",
            "-n",
            "/abs/path",
            "two words",
            "all;rm",
            "a$(b)",
            "",
            "rule\n",
            "caf\u{e9}",
        ] {
            assert!(Engine::Snakemake.target_problem(bad).is_some(), "{bad:?}");
        }
        assert_eq!(Engine::Nextflow.target_problem("ALIGN_ONLY"), None);
        for bad in ["-resume", "9lives", "a.b", "main:sub", "a-b"] {
            assert!(Engine::Nextflow.target_problem(bad).is_some(), "{bad:?}");
        }
        assert_eq!(
            Engine::Nextflow.target_problem("-resume").unwrap(),
            "a nextflow target must be a workflow name (ASCII letters, digits and `_`, not \
             starting with a digit); got \"-resume\""
        );
        let long = "a".repeat(MAX_TARGET_CHARS + 1);
        assert_eq!(
            Engine::Snakemake.target_problem(&long).unwrap(),
            "a snakemake target is at most 256 characters"
        );
    }

    #[test]
    fn an_engine_s_name_is_its_serde_form_both_ways() {
        // `ALL` is every variant: this match has no wildcard, so a new engine fails to compile here
        // until it is added to `ALL` (and so reaches as_str/FromStr/serde).
        for engine in Engine::ALL {
            match engine {
                Engine::Snakemake | Engine::Nextflow => {}
            }
            let wire = serde_json::to_value(engine).unwrap();
            assert_eq!(wire.as_str(), Some(engine.as_str()));
            assert_eq!(engine.as_str().parse::<Engine>(), Ok(engine));
            assert_eq!(serde_json::from_value::<Engine>(wire).unwrap(), engine);
            assert_eq!(engine.to_string(), engine.as_str());
        }
        assert_eq!("cromwell".parse::<Engine>(), Err(UnknownEngine("cromwell".into())));
        assert_eq!(Engine::ALL.len(), 2);
    }

    #[test]
    fn a_nextflow_release_in_range_must_be_one_biffix_can_build() {
        for release in NEXTFLOW_RELEASES {
            assert!(check_engine_version("nextflow", release).is_ok(), "{release}");
        }
        assert_eq!(
            check_engine_version("nextflow", "25.10.5").unwrap_err().to_string(),
            "nextflow 25.10.5 is not a release biffix can build; pin one of 25.10.4"
        );
        // The month's spelling doesn't matter: `25.10.04` is `25.10.4`.
        assert!(check_engine_version("nextflow", "25.10.04").is_ok());
    }

    #[test]
    fn a_pin_that_is_not_exact_is_echoed_trimmed() {
        // A YAML value's surrounding whitespace (a trailing newline) is not shown inside the quotes.
        match check_engine_version("snakemake", " 9\n") {
            Err(EngineVersionError::NotExact { given, .. }) => assert_eq!(given, "9"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_retired_release_is_refused_by_name_with_its_replacement() {
        // However the month is spelled (`25.4.0` is `25.04.0`).
        for given in ["25.04.0", "25.4.0", " 25.04.0\n"] {
            assert_eq!(
                check_engine_version("nextflow", given).unwrap_err().to_string(),
                "Nextflow 25.04.0 is no longer supported; re-register on 25.10.4",
                "{given:?}"
            );
        }
        assert_eq!(retired("snakemake", "25.4.0"), None);
        assert_eq!(retired("nextflow", "25.10.4"), None);
        // A replacement is a release biffix can build, and a retired release is not one.
        for r in RETIRED {
            let range = supported_range(r.engine.as_str()).unwrap();
            assert!(check_engine_version(r.engine.as_str(), r.replacement).is_ok());
            assert!(!range.releases.contains(&r.release), "{}", r.release);
        }
    }

    #[test]
    fn parses_only_exact_releases() {
        assert_eq!(EngineVersion::parse("25.10.4"), Some(EngineVersion::new(25, 10, 4)));
        for bad in ["9", "9.3", "9.3.0.1", ">=9", "9.x.0", "25.10.0-edge", "", "9..0", " 9.3.0"] {
            assert_eq!(EngineVersion::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_supported_bound_is_an_exact_version_and_the_range_is_non_empty() {
        for range in SUPPORTED {
            let (min, below) = (
                EngineVersion::parse(range.min).unwrap(),
                EngineVersion::parse(range.below).unwrap(),
            );
            assert!(min < below, "{}", range.engine);
            assert!(range.contains(min) && !range.contains(below));
        }
    }

    #[test]
    fn versions_order_numerically_not_lexically() {
        assert!(EngineVersion::new(9, 10, 0) > EngineVersion::new(9, 9, 9));
        assert!(EngineVersion::new(25, 4, 0) < EngineVersion::new(25, 10, 4));
    }

    #[test]
    fn accepts_releases_inside_the_range_including_its_minimum() {
        assert_eq!(
            check_engine_version("snakemake", "9.0.0").map(|p| p.version),
            Ok(EngineVersion::new(9, 0, 0))
        );
        assert!(check_engine_version("snakemake", "9.99.3").is_ok());
        assert!(check_engine_version("nextflow", "25.10.4").is_ok());
        // Surrounding whitespace from a YAML value is tolerated.
        assert!(check_engine_version("nextflow", " 25.10.4\n").is_ok());
    }

    #[test]
    fn rejects_releases_outside_the_range() {
        for (engine, version) in [
            ("snakemake", "8.30.0"),
            ("snakemake", "10.0.0"),
            ("nextflow", "24.10.5"),
            ("nextflow", "26.0.0"),
        ] {
            let err = check_engine_version(engine, version).unwrap_err();
            assert!(
                matches!(err, EngineVersionError::Unsupported { .. }),
                "{engine} {version}: {err}"
            );
        }
        assert_eq!(
            check_engine_version("snakemake", "10.0.0").unwrap_err().to_string(),
            "snakemake 10.0.0 is not supported; biffix supports snakemake >=9.0.0, <10.0.0"
        );
    }

    #[test]
    fn rejects_a_non_exact_pin_with_an_example() {
        let err = check_engine_version("snakemake", "9").unwrap_err();
        assert_eq!(
            err.to_string(),
            "snakemake engine_version `9` is not an exact release; pin one like `9.0.0` \
             (supported: >=9.0.0, <10.0.0)"
        );
    }

    #[test]
    fn reports_nextflow_versions_as_nextflow_writes_them() {
        assert_eq!(
            check_engine_version("nextflow", "24.10.5").unwrap_err().to_string(),
            "nextflow 24.10.5 is not supported; biffix supports nextflow >=25.10.4, <26.0.0"
        );
    }

    #[test]
    fn a_pin_is_spelled_one_way() {
        for (engine, given, canonical) in [
            ("snakemake", "9.03.0", "9.3.0"),
            ("snakemake", "9.3.0", "9.3.0"),
            ("snakemake", " 9.27.00\n", "9.27.0"),
            ("nextflow", "25.10.04", "25.10.4"),
            ("nextflow", "25.10.4", "25.10.4"),
        ] {
            let pinned = check_engine_version(engine, given).unwrap();
            assert_eq!(pinned.canonical, canonical, "{engine} {given:?}");
            assert_eq!(spelling(engine, pinned.version), canonical);
        }
        // Nextflow zero-pads the month.
        assert_eq!(spelling("nextflow", EngineVersion::new(25, 4, 0)), "25.04.0");
    }

    #[test]
    fn rejects_an_unknown_engine() {
        assert_eq!(
            check_engine_version("cromwell", "1.0.0"),
            Err(EngineVersionError::UnknownEngine("cromwell".into()))
        );
    }
}
