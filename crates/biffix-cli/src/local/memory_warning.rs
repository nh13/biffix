//! The advisory `inspect` and `register` print for a Snakemake workflow whose rules would run on
//! AWS Batch with Snakemake's default memory ([`biffix_workflow::scan::resources`]). It never fails
//! the command.

use std::path::Path;

use biffix_workflow::engines::EngineVersion;
use biffix_workflow::scan::resources::{scan_snakemake_resources, ResourceScan};

use crate::local::source::SourceFiles;

/// How many rules the warning names before counting the rest.
const SHOWN: usize = 5;

/// Print (to stderr) the warning for the Snakemake workflow at `root`, whose source the coordinator
/// gets is `files`, run from `entrypoint` (a file of `files`) on `engine_version` (`None`: not
/// pinned, so every supported release is assumed), if any rule would run with Snakemake's default
/// memory.
pub(crate) fn warn_rules_without_memory(
    root: &Path,
    files: &SourceFiles,
    entrypoint: &str,
    engine_version: Option<&str>,
) {
    if let Some(warning) = memory_warning(root, files, entrypoint, engine_version) {
        eprintln!("[biffix] {warning}");
    }
}

/// The warning for the Snakemake workflow at `root`, if there is one. Reads only the files of
/// `files` the check needs (the Snakefile, what it includes, the workflow profile).
fn memory_warning(
    root: &Path,
    files: &SourceFiles,
    entrypoint: &str,
    engine_version: Option<&str>,
) -> Option<String> {
    let paths = files.paths();
    // Read as the run reads it: a symlink is followed, as Snakemake follows it.
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let version = engine_version.and_then(|v| EngineVersion::parse(v.trim()));
    warning_text(&scan_snakemake_resources(&paths, read, entrypoint, version))
}

/// The warning for `scan`, if any rule is listed.
fn warning_text(scan: &ResourceScan) -> Option<String> {
    let n = scan.rules_without_mem.len();
    if n == 0 {
        return None;
    }
    let shown: Vec<String> = scan
        .rules_without_mem
        .iter()
        .take(SHOWN)
        .map(|r| {
            let ignored = r
                .ignored_key
                .as_ref()
                .map_or(String::new(), |k| format!(" ({k} is not read as memory)"));
            format!("{}:{} {}{ignored}", r.path, r.line, r.rule)
        })
        .collect();
    let more = if n > SHOWN { format!(", … and {} more", n - SHOWN) } else { String::new() };
    let mut text = format!(
        "warning: {n} rule(s) set no memory, and the workflow profile sets no default-resources \
         memory: {}{more}. On AWS Batch each gets Snakemake's default mem_mb, \
         min(max(2×input, 1000), 8000) MB, and the AWS Batch executor will not submit a job under \
         1024 MB, so one whose inputs total under 512 MB fails to submit. Set default-resources: \
         [mem_mb=…] in {} (or resources: mem_mb=… per rule).",
        shown.join(", "),
        scan.profile_hint
    );
    if let Some(profile) = &scan.unreadable_profile {
        text.push_str(&format!(" {profile} could not be parsed, so it was taken to set nothing."));
    }
    if !scan.unchecked_modules.is_empty() {
        text.push_str(&format!(
            " Rules imported from module(s) {} were not checked.",
            scan.unchecked_modules.join(", ")
        ));
    }
    if scan.every_release {
        text.push_str(
            " biffix.yaml pins no engine_version, so every Snakemake 9 release was assumed.",
        );
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::source::shipped_files;
    use crate::local::source::SourceMode;
    use crate::test_git::git_ok;
    use biffix_workflow::scan::resources::RuleLoc;

    fn rule(i: usize) -> RuleLoc {
        RuleLoc {
            path: "workflow/Snakefile".into(),
            line: 3 * i + 1,
            rule: format!("r{i}"),
            ignored_key: None,
        }
    }

    fn scan(n: usize) -> ResourceScan {
        ResourceScan {
            rules_without_mem: (0..n).map(rule).collect(),
            profile_hint: "workflow/profiles/default/config.yaml".into(),
            ..ResourceScan::default()
        }
    }

    #[test]
    fn the_warning_names_the_first_five_rules_and_counts_the_rest() {
        assert_eq!(warning_text(&scan(0)), None);
        assert_eq!(
            warning_text(&scan(7)).unwrap(),
            "warning: 7 rule(s) set no memory, and the workflow profile sets no default-resources \
             memory: workflow/Snakefile:1 r0, workflow/Snakefile:4 r1, workflow/Snakefile:7 r2, \
             workflow/Snakefile:10 r3, workflow/Snakefile:13 r4, … and 2 more. On AWS Batch each \
             gets Snakemake's default mem_mb, min(max(2×input, 1000), 8000) MB, and the AWS Batch \
             executor will not submit a job under 1024 MB, so one whose inputs total under 512 MB \
             fails to submit. Set default-resources: [mem_mb=…] in \
             workflow/profiles/default/config.yaml (or resources: mem_mb=… per rule)."
        );
        // Exactly five: all named, nothing counted.
        let five = warning_text(&scan(5)).unwrap();
        assert!(five.contains("workflow/Snakefile:13 r4. On AWS Batch"), "{five}");
        assert!(!five.contains("more"), "{five}");
    }

    #[test]
    fn the_warning_names_an_ignored_key_and_what_was_not_checked() {
        let mut scan = scan(1);
        scan.rules_without_mem[0].ignored_key = Some("mem_gb".into());
        scan.unreadable_profile = Some("workflow/profiles/default/config.yaml".into());
        scan.unchecked_modules = vec!["dna".into(), "rna".into()];
        scan.every_release = true;
        let text = warning_text(&scan).unwrap();
        for part in [
            "memory: workflow/Snakefile:1 r0 (mem_gb is not read as memory). On AWS Batch",
            " workflow/profiles/default/config.yaml could not be parsed, so it was taken to set \
             nothing.",
            " Rules imported from module(s) dna, rna were not checked.",
            " biffix.yaml pins no engine_version, so every Snakemake 9 release was assumed.",
        ] {
            assert!(text.contains(part), "{part:?} in {text}");
        }
    }

    /// `register` checks the entrypoint it records (`Snakefile` by default) on its pinned release,
    /// reading the tracked files only.
    #[test]
    fn register_s_check_reads_the_recorded_entrypoint_s_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_ok(root, &["init", "-q"]);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "Snakefile",
            "include: \"rules/a.smk\"\nrule top:\n    output: \"t\"\n    resources: mem_mib=10\n    shell: \"x\"\n",
        );
        write("rules/a.smk", "rule a:\n    output: \"a\"\n    shell: \"x\"\n");
        git_ok(root, &["add", "Snakefile", "rules/a.smk"]);
        // Untracked: not part of the version, so not read.
        write("profiles/default/config.yaml", "default-resources: [mem_mb=1]\n");

        let listed = || shipped_files(root, SourceMode::Git, None).unwrap();
        let warning = memory_warning(root, &listed(), "Snakefile", Some("9.27.0")).unwrap();
        assert!(warning.starts_with("warning: 1 rule(s) set no memory"), "{warning}");
        assert!(warning.contains(": rules/a.smk:1 a. "), "{warning}");
        assert!(!warning.contains("engine_version"), "{warning}");
        // On 9.0.0 `mem_mib` is not read either.
        let warning = memory_warning(root, &listed(), "Snakefile", Some("9.0.0")).unwrap();
        assert!(
            warning.contains(": Snakefile:2 top (mem_mib is not read as memory), rules/a.smk:1 a."),
            "{warning}"
        );

        git_ok(root, &["add", "profiles/default/config.yaml"]);
        assert_eq!(memory_warning(root, &listed(), "Snakefile", Some("9.27.0")), None);
    }

    #[test]
    fn a_staged_source_s_submodule_files_are_read() {
        // With `s3` the coordinator gets a populated submodule's files, so the check reads them.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let files = crate::local::source::SourceFiles::of(&["Snakefile", "lib/rules.smk"]);
        std::fs::create_dir(root.join("lib")).unwrap();
        std::fs::write(root.join("Snakefile"), "include: \"lib/rules.smk\"\n").unwrap();
        std::fs::write(
            root.join("lib/rules.smk"),
            "rule a:\n    output: \"a\"\n    shell: \"x\"\n",
        )
        .unwrap();
        let warning = memory_warning(root, &files, "Snakefile", Some("9.27.0")).unwrap();
        assert!(warning.contains(": lib/rules.smk:1 a. "), "{warning}");
        let without = crate::local::source::SourceFiles::of(&["Snakefile"]);
        assert_eq!(memory_warning(root, &without, "Snakefile", Some("9.27.0")), None);
    }
}
