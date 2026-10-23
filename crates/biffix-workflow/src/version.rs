//! Version identity: the normalized workflow source tree, the deterministic content hash, the
//! `{git_sha}:{content_hash}` source identity, and the `{git_sha}:{content_hash}:{build}` version
//! id.
//!
//! It lives in biffix-workflow, not the backend, so the CLI and the backend compute the
//! version id with the same function — identical source ⇒ identical version on both sides (the
//! "version identity" correctness property). The backend keeps `freeze_version`/`RegisterInput`
//! (which depend on backend-only domain + image types) and calls [`build_version_id`] from them.

use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

/// One git-tracked entry the version id covers: a regular file (read from disk), a symlink (index
/// mode 120000), or a submodule gitlink (index mode 160000), which is a *directory* on disk and so
/// cannot be read as a file — its identity is the recorded commit SHA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// Repo-root-relative path.
    pub path: String,
    /// `Some(commit_sha)` for a submodule gitlink; `None` otherwise.
    pub gitlink_sha: Option<String>,
    /// `Some(target)` for a symlink: the link's own text, which is what git stores for it. The
    /// link is never read through (its target may be a directory, or outside the repo).
    pub symlink_target: Option<String>,
}

impl SourceEntry {
    /// A regular (non-submodule, non-symlink) tracked file.
    pub fn file(path: impl Into<String>) -> Self {
        Self { path: path.into(), gitlink_sha: None, symlink_target: None }
    }
}

/// What kind of tree entry a file is, as far as the staged tarball (and so the hash) can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// A regular file, staged 0644.
    Regular,
    /// A regular file with an executable bit, staged 0755.
    Executable,
    /// A symlink, staged as a link; its bytes are the link's target text.
    Symlink,
}

/// One tracked file as the version covers it: its kind and its bytes (a symlink's target text, a
/// gitlink's commit SHA, or a file's contents).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeFile {
    pub path: String,
    pub kind: FileKind,
    pub bytes: Vec<u8>,
}

/// Read one tracked file at `path` as the version and the staged tarball both see it: a symlink
/// (named by `symlink_target`, or found on disk) is its target text, never followed; a regular
/// file is its bytes and whether any executable bit is set (never, on a host with no exec bit).
pub fn read_tree_file(
    path: &Path,
    symlink_target: Option<&str>,
) -> io::Result<(FileKind, Vec<u8>)> {
    if let Some(target) = symlink_target {
        return Ok((FileKind::Symlink, target.as_bytes().to_vec()));
    }
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        return Ok((FileKind::Symlink, target.to_string_lossy().into_owned().into_bytes()));
    }
    let bytes = std::fs::read(path)?;
    let kind = if is_executable(&meta) { FileKind::Executable } else { FileKind::Regular };
    Ok((kind, bytes))
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Normalize git-tracked `entries` (repo-root-relative) under `root` into [`TreeFile`]s sorted by
/// path, defensively excluding anything under `.git/`. Ordering is normalized here so the content
/// hash is independent of the order entries are supplied in.
///
/// A regular file contributes its on-disk bytes and executable bit. A symlink contributes its
/// target text (as git stores it), without being followed. A submodule gitlink contributes its
/// recorded commit SHA as bytes — it is a directory on disk, so reading it would fail (`Is a
/// directory`), and the pinned commit is exactly what identifies the submodule, so a submodule
/// bump correctly changes the version id.
///
/// The ONE definition of *which* bytes the version id covers, so the CLI and any other producer
/// agree — version identity is meaningless if two sides disagree on the file set. The caller
/// enumerates git-tracked entries (`git ls-files -s`, which never lists `.git/`) and passes them,
/// so this function has no git/process dependency and is fully unit-testable. Feed its output
/// straight into [`tree_hash`].
pub fn normalized_tree(root: &Path, entries: &[SourceEntry]) -> io::Result<Vec<TreeFile>> {
    let mut files = Vec::with_capacity(entries.len());
    for entry in entries {
        // `.git/` is never workflow source; skip it even if a caller passes it in.
        if entry.path == ".git" || entry.path.starts_with(".git/") {
            continue;
        }
        let (kind, bytes) = match &entry.gitlink_sha {
            Some(sha) => (FileKind::Regular, sha.clone().into_bytes()),
            None => read_tree_file(&root.join(&entry.path), entry.symlink_target.as_deref())?,
        };
        files.push(TreeFile { path: entry.path.clone(), kind, bytes });
    }
    // Sort by path so the set — and therefore the content hash — is canonical regardless of the
    // order the caller enumerated the tree.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// [`normalized_tree`] as sorted `(path, bytes)` pairs, for readers that want only the text (the
/// image scan). A symlink's bytes are its target text.
pub fn normalized_files(
    root: &Path,
    entries: &[SourceEntry],
) -> io::Result<Vec<(String, Vec<u8>)>> {
    Ok(normalized_tree(root, entries)?.into_iter().map(|f| (f.path, f.bytes)).collect())
}

/// Deterministic content hash over the sorted (path, sha256(bytes), kind) entries of a tree.
///
/// Each entry is `path NUL sha256(bytes)`, followed by `NUL x` for an executable file or `NUL l`
/// for a symlink (paths never contain NUL, so the framing is unambiguous). The hash names the
/// staged tarball, and the tarball keeps the executable bit and symlinks, so they are part of it
/// (as they are of a git tree): a `chmod +x` fix to a build context's script is a new build, not
/// the cached image. A tree of plain files hashes exactly as it did before kinds were covered.
///
/// The bit is read from the working tree (as the tarball reads it), not from git's index, so an
/// uncommitted `chmod` registered with `--allow-dirty` is part of the identity. The cost: a host
/// with no executable bit (Windows) stages and hashes every file as plain, so the same commit has
/// another identity there — as its tarball really does differ.
pub fn tree_hash(mut files: Vec<TreeFile>) -> String {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    for f in &files {
        h.update(f.path.as_bytes());
        h.update([0u8]);
        h.update(Sha256::digest(&f.bytes));
        match f.kind {
            FileKind::Regular => {}
            FileKind::Executable => h.update(b"\0x"),
            FileKind::Symlink => h.update(b"\0l"),
        }
    }
    hex::encode(h.finalize())
}

/// [`tree_hash`] over plain (non-executable, non-symlink) files.
pub fn content_hash(files: Vec<(String, Vec<u8>)>) -> String {
    tree_hash(
        files
            .into_iter()
            .map(|(path, bytes)| TreeFile { path, kind: FileKind::Regular, bytes })
            .collect(),
    )
}

/// The source identity `{git_sha}:{content_hash}`: identical source ⇒ identical value. It
/// names a staged source tarball; a version id adds the build number ([`build_version_id`]).
pub fn source_id(git_sha: &str, content_hash: &str) -> String {
    format!("{git_sha}:{content_hash}")
}

/// A version id: the source identity plus a bioconda-style build number,
/// `{git_sha}:{content_hash}:{build}`. Registering the same source again with different inputs
/// (image, engine version, …) freezes the next build (0, 1, 2, …) instead of colliding.
pub fn build_version_id(git_sha: &str, content_hash: &str, build: u32) -> String {
    format!("{git_sha}:{content_hash}:{build}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty scratch directory for one test.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("biffix-nf-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, bytes: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn normalizes_order_and_excludes_git() {
        let dir = scratch("order");
        write(&dir, "Snakefile", b"rule all:\n");
        write(&dir, "config/config.yaml", b"reads: x\n");
        write(&dir, ".git/HEAD", b"ref: refs/heads/main\n");

        // Supplied out of order and including a `.git/` path.
        let supplied = vec![
            SourceEntry::file("config/config.yaml"),
            SourceEntry::file(".git/HEAD"),
            SourceEntry::file("Snakefile"),
        ];
        let out = normalized_files(&dir, &supplied).unwrap();

        // `.git/` excluded, remaining pairs sorted by path.
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["Snakefile", "config/config.yaml"]);

        // Order-independent: a different supply order yields the identical set (and hash).
        let reordered =
            vec![SourceEntry::file("Snakefile"), SourceEntry::file("config/config.yaml")];
        assert_eq!(out, normalized_files(&dir, &reordered).unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = scratch("miss");
        let err = normalized_files(&dir, &[SourceEntry::file("nope.txt")]);
        assert!(err.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submodule_gitlink_hashes_its_sha_without_reading_a_directory() {
        // A gitlink's path is a directory on disk (or absent); it must NOT be read as a file. Its
        // recorded commit SHA is the content, so its presence and its SHA are covered by the hash.
        let dir = scratch("gitlink");
        write(&dir, "Snakefile", b"rule all:\n");
        // The gitlink dir literally exists on disk — proving we never fs::read it.
        std::fs::create_dir_all(dir.join("vendor/minibwa")).unwrap();

        let gitlink = |sha: &str| SourceEntry {
            path: "vendor/minibwa".to_string(),
            gitlink_sha: Some(sha.to_string()),
            symlink_target: None,
        };
        let with_a = vec![SourceEntry::file("Snakefile"), gitlink("a".repeat(40).as_str())];
        let out = normalized_files(&dir, &with_a).expect("gitlink must not be read as a file");
        assert_eq!(
            out.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            vec!["Snakefile", "vendor/minibwa"]
        );

        // A submodule bump (different pinned SHA) changes the content hash — provenance is covered.
        let with_b = vec![SourceEntry::file("Snakefile"), gitlink("b".repeat(40).as_str())];
        assert_ne!(
            content_hash(normalized_files(&dir, &with_a).unwrap()),
            content_hash(normalized_files(&dir, &with_b).unwrap()),
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_its_target_text_never_followed() {
        let dir = scratch("link");
        let root = dir.as_path();
        let outside = scratch("link-outside");
        std::fs::write(outside.join("hosts"), b"machine-local bytes\n").unwrap();
        std::fs::create_dir_all(root.join("shared/envs")).unwrap();
        write(root, "shared/envs/bwa.yaml", b"deps: [bwa]\n");
        // A link to a directory (reading through it fails with `Is a directory`), and one to a
        // file outside the repo (reading through it hashes whatever this machine holds there).
        std::os::unix::fs::symlink("shared/envs", root.join("envs")).unwrap();
        let abs = outside.join("hosts");
        std::os::unix::fs::symlink(&abs, root.join("hosts")).unwrap();

        let marked = |path: &str, target: &str| SourceEntry {
            symlink_target: Some(target.to_string()),
            ..SourceEntry::file(path)
        };
        let abs_text = abs.to_string_lossy().into_owned();
        let tree =
            normalized_tree(root, &[marked("envs", "shared/envs"), marked("hosts", &abs_text)])
                .expect("a symlink must not be read through");
        assert_eq!(
            tree,
            vec![
                TreeFile {
                    path: "envs".into(),
                    kind: FileKind::Symlink,
                    bytes: b"shared/envs".to_vec(),
                },
                TreeFile {
                    path: "hosts".into(),
                    kind: FileKind::Symlink,
                    bytes: abs_text.clone().into_bytes(),
                },
            ]
        );
        // A caller that did not mark them gets the same: the link is found on disk.
        let unmarked =
            normalized_tree(root, &[SourceEntry::file("envs"), SourceEntry::file("hosts")])
                .unwrap();
        assert_eq!(unmarked, tree);
        // Changing what the outside file holds does not change the version.
        let before = tree_hash(tree);
        std::fs::write(&abs, b"other bytes\n").unwrap();
        let after = tree_hash(
            normalized_tree(root, &[SourceEntry::file("envs"), SourceEntry::file("hosts")])
                .unwrap(),
        );
        assert_eq!(before, after);
        // A link is not the same entry as a file holding its target text.
        write(root, "plain", b"shared/envs");
        let as_file = tree_hash(normalized_tree(root, &[SourceEntry::file("plain")]).unwrap());
        std::fs::remove_file(root.join("plain")).unwrap();
        std::os::unix::fs::symlink("shared/envs", root.join("plain")).unwrap();
        let as_link = tree_hash(normalized_tree(root, &[SourceEntry::file("plain")]).unwrap());
        assert_ne!(as_file, as_link);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_is_part_of_the_hash() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("exec");
        let root = dir.as_path();
        write(root, "run.sh", b"#!/bin/sh\n");
        let entries = [SourceEntry::file("run.sh")];
        let mode = |m: u32| {
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(m))
                .unwrap();
        };
        mode(0o644);
        let plain = normalized_tree(root, &entries).unwrap();
        assert_eq!(plain[0].kind, FileKind::Regular);
        // A plain file hashes as it always has (as a bare `(path, bytes)` pair).
        assert_eq!(
            tree_hash(plain.clone()),
            content_hash(normalized_files(root, &entries).unwrap())
        );
        mode(0o755);
        let exec = normalized_tree(root, &entries).unwrap();
        assert_eq!(exec[0].kind, FileKind::Executable);
        assert_ne!(tree_hash(plain), tree_hash(exec));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The stored identities, pinned to literal values computed independently of this code (a
    /// Python sha256 over the documented framing). Every version id and build key in every tenant
    /// is made from these: changing one is a deliberate, announced change to every identity.
    #[test]
    fn the_hashes_are_pinned() {
        let plain = vec![
            ("Snakefile".to_string(), b"rule all:\n".to_vec()),
            ("config/config.yaml".to_string(), b"reads: x\n".to_vec()),
        ];
        assert_eq!(
            content_hash(plain),
            "4154d24779fab2f5395548db5e779c5e1f59123001beb644b0bf154699911a45"
        );
        let file = |path: &str, kind, bytes: &[u8]| TreeFile {
            path: path.into(),
            kind,
            bytes: bytes.to_vec(),
        };
        assert_eq!(
            tree_hash(vec![
                file("run.sh", FileKind::Executable, b"#!/bin/sh\n"),
                file("envs", FileKind::Symlink, b"../shared/envs"),
                file("Snakefile", FileKind::Regular, b"rule all:\n"),
            ]),
            "cc23e271ccfb6e7aec7c6fb9be5bf4d34dac688f9226a4e2189547dabd247830"
        );
        assert_eq!(source_id("abc", "def"), "abc:def");
        assert_eq!(build_version_id("abc", "def", 2), "abc:def:2");
    }
}
