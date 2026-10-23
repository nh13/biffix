//! Git resolution and the normalized source file set → content hash.
//!
//! The impure part (shelling out to `git`) lives here; the *definition* of which bytes the version
//! id covers is `biffix_workflow::version::normalized_files`, shared with any other producer so
//! version identity cannot drift.

use std::path::Path;
use std::process::Command;

use biffix_workflow::version::{FileKind, SourceEntry, TreeFile};

use crate::error::{CliError, CliResult};

/// How the coordinator fetches a version's source at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SourceMode {
    /// Record the git origin; the coordinator anonymously clones it (public repos only).
    Git,
    /// Stage an immutable source tarball to the project bucket; the coordinator pulls it from S3
    /// (required for private repos; no GitHub credential reaches the coordinator).
    S3,
}

impl SourceMode {
    /// The wire value recorded in a version's registration inputs.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SourceMode::Git => "git",
            SourceMode::S3 => "s3",
        }
    }
}

/// Resolved git provenance for a workflow tree.
#[derive(Debug, Clone)]
pub(crate) struct GitInfo {
    /// `origin`'s URL, with any credentials removed (see [`strip_userinfo`]).
    pub(crate) origin: Option<String>,
    /// The host whose credentials were removed from `origin`, if any (for a warning that names
    /// the host, never the credential).
    pub(crate) origin_credentials_removed: Option<String>,
    pub(crate) git_ref: Option<String>,
    pub(crate) sha: String,
    /// Uncommitted changes to tracked files under the workflow root. Untracked files are never
    /// hashed or staged, so they do not count, and neither do changes outside the root.
    pub(crate) dirty: bool,
    /// The workflow root's path within its repository (`git rev-parse --show-prefix`, e.g.
    /// `workflows/align/`); empty at the repository's top level.
    pub(crate) prefix: String,
    /// Submodules under the root whose checkout differs from the commit the parent records
    /// (another commit checked out, or edits to its tracked files), root-relative.
    pub(crate) modified_submodules: Vec<String>,
}

fn run_git(root: &Path, args: &[&str]) -> CliResult<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| CliError::InvalidInput(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(CliError::InvalidInput(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// A git command's output as one trimmed line of text (a SHA, a ref, a URL).
fn git(root: &Path, args: &[&str]) -> CliResult<String> {
    Ok(String::from_utf8_lossy(&run_git(root, args)?).trim().to_string())
}

fn git_opt(root: &Path, args: &[&str]) -> Option<String> {
    git(root, args).ok().filter(|s| !s.is_empty())
}

/// A `-z` git listing's NUL-terminated records, verbatim (paths unquoted, nothing trimmed; the
/// empty piece after the last terminator is not a record). A record that is not UTF-8 is an error
/// rather than a silently altered path.
fn nul_records(root: &Path, args: &[&str]) -> CliResult<Vec<String>> {
    run_git(root, args)?
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| {
            String::from_utf8(r.to_vec()).map_err(|_| {
                CliError::InvalidInput(format!(
                    "`git {}` lists a path that is not UTF-8 ({}); rename it",
                    args.join(" "),
                    String::from_utf8_lossy(r)
                ))
            })
        })
        .collect()
}

/// `url` with its credentials removed, plus the host they were removed for: an http(s) URL's
/// userinfo (a PAT or a CI job token cloned into the origin), and any other URL's password
/// (`ssh://user:password@host/…`). A user without a password elsewhere — an scp-style
/// `git@host:owner/repo`, or an `ssh://git@host/…` — is not a secret, and is kept.
fn strip_userinfo(url: &str) -> (String, Option<String>) {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return (url.to_string(), None);
    };
    let http = matches!(parsed.scheme(), "http" | "https");
    let secret = parsed.password().is_some() || (http && !parsed.username().is_empty());
    if !secret {
        return (url.to_string(), None);
    }
    let host = parsed.host_str().unwrap_or_default().to_string();
    // Neither fails for a URL with a host, which one with userinfo has.
    let _ = parsed.set_password(None);
    if http {
        let _ = parsed.set_username("");
    }
    (parsed.to_string(), Some(host))
}

/// What `git status` says about the tracked files under a workflow root.
#[derive(Debug, Default, PartialEq)]
struct Status {
    dirty: bool,
    modified_submodules: Vec<String>,
}

/// Read `git status --porcelain=v2` for the tracked files under `root`. Untracked files are left
/// out (they are never hashed or staged), and a submodule's `ignore =` setting is overridden: a
/// staged tarball snapshots its checkout whatever the setting says.
fn status(root: &Path) -> CliResult<Status> {
    let records = nul_records(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=none",
            "--",
            ".",
        ],
    )?;
    Ok(parse_status(&records))
}

/// [`status`]'s reading of `git status --porcelain=v2 -z` records.
fn parse_status(records: &[String]) -> Status {
    let mut out = Status::default();
    let mut records = records.iter();
    while let Some(record) = records.next() {
        let kind = record.split(' ').next().unwrap_or("");
        match kind {
            // `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`; a rename or copy (`2`) has a score
            // before its path, and its original path as the next record.
            "1" | "2" => {
                let fields = if kind == "1" { 9 } else { 10 };
                let cols: Vec<&str> = record.splitn(fields, ' ').collect();
                if kind == "2" {
                    records.next();
                }
                let xy = cols.get(1).copied().unwrap_or("..").as_bytes();
                let sub = cols.get(2).copied().unwrap_or("N...").as_bytes();
                let path = cols.get(fields - 1).copied().unwrap_or_default();
                let staged = xy.first().is_some_and(|x| *x != b'.');
                let unstaged = xy.get(1).is_some_and(|y| *y != b'.');
                if sub.first() == Some(&b'S') {
                    // `S<c><m><u>`: another commit checked out, tracked edits, untracked files.
                    // A submodule's untracked files are never staged, so they are no change.
                    let moved = sub.get(1) == Some(&b'C') || sub.get(2) == Some(&b'M');
                    if moved {
                        out.modified_submodules.push(path.to_string());
                    }
                    out.dirty |= staged || moved;
                } else {
                    out.dirty |= staged || unstaged;
                }
            }
            // An unmerged path.
            "u" => out.dirty = true,
            _ => {}
        }
    }
    out
}

/// Resolve origin URL, current ref, HEAD SHA, where the root sits in its repository, and whether
/// the tree is dirty.
pub(crate) fn resolve_git(root: &Path) -> CliResult<GitInfo> {
    // Confirm we are inside a work tree first, for a clear error rather than a cascade.
    if git(root, &["rev-parse", "--is-inside-work-tree"])? != "true" {
        return Err(CliError::InvalidInput(format!(
            "{} is not inside a git work tree",
            root.display()
        )));
    }
    let sha = git(root, &["rev-parse", "HEAD"])?;
    let git_ref = git_opt(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let (origin, origin_credentials_removed) = match git_opt(root, &["remote", "get-url", "origin"])
    {
        Some(url) => {
            let (url, host) = strip_userinfo(&url);
            (Some(url), host)
        }
        None => (None, None),
    };
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let Status { dirty, mut modified_submodules } = status(root)?;
    // `git status` names paths from the repository's top level; `-- .` keeps them under the root.
    for path in &mut modified_submodules {
        if let Some(rel) = path.strip_prefix(prefix.as_str()) {
            *path = rel.to_string();
        }
    }
    Ok(GitInfo {
        origin,
        origin_credentials_removed,
        git_ref,
        sha,
        dirty,
        prefix,
        modified_submodules,
    })
}

/// Whether HEAD is on a remote-tracking branch, as far as this clone knows: as of its last fetch,
/// so a commit pushed from elsewhere reads as absent until `git fetch`. `None` when git cannot say.
/// Nothing is fetched (no network, no credential prompt).
pub(crate) fn head_on_a_remote_branch(root: &Path) -> Option<bool> {
    git(root, &["branch", "-r", "--contains", "HEAD"]).ok().map(|branches| !branches.is_empty())
}

/// The git-tracked entries under `root`, root-relative, from `git ls-files -s -z` — so a path git
/// would C-quote (non-ASCII, a quote, a backslash, a tab) comes through verbatim. The `-s` (stage)
/// form yields `<mode> <object> <stage>\t<path>`, so a submodule gitlink (mode 160000) is
/// distinguished from a regular file — the version hash records its recorded commit SHA rather than
/// trying to read the submodule directory (which fails with `Is a directory`) — and so is a
/// symlink (mode 120000), which is recorded as its target text (as git stores it) rather than read
/// through: its target may be a directory, or a file outside the repo.
pub(crate) fn tracked_files(root: &Path) -> CliResult<Vec<SourceEntry>> {
    let mut entries = Vec::new();
    for record in nul_records(root, &["ls-files", "-s", "-z"])? {
        let (meta, path) = record.split_once('\t').ok_or_else(|| {
            CliError::InvalidInput(format!("unexpected `git ls-files -s` output: {record}"))
        })?;
        // Inside an unpopulated submodule's (empty) directory, git lists the parent's gitlink for
        // it as `./`: that names no file of the directory's own.
        if path.is_empty() || path == "./" {
            continue;
        }
        let mut cols = meta.split_whitespace();
        let mode = cols.next().unwrap_or("");
        let object = cols.next().unwrap_or("");
        let gitlink_sha = (mode == "160000").then(|| object.to_string());
        let symlink_target = if mode == "120000" {
            let target = std::fs::read_link(root.join(path)).map_err(|e| {
                CliError::InvalidInput(format!("cannot read the tracked symlink {path}: {e}"))
            })?;
            Some(target.to_string_lossy().into_owned())
        } else {
            None
        };
        entries.push(SourceEntry { path: path.to_string(), gitlink_sha, symlink_target });
    }
    Ok(entries)
}

/// One tracked file to stage: its path in the tarball, where it is on disk, and (for a symlink)
/// the link's target text.
#[derive(Debug, Clone)]
struct Tracked {
    rel: String,
    abs: std::path::PathBuf,
    symlink_target: Option<String>,
}

/// A workflow's tracked files as staged tarballs see them: every tracked file at its root-relative
/// path, **recursing into populated submodules** so their tracked source is staged at its full path
/// too, plus the submodules that are not checked out (nothing to stage). Listed once per
/// `register` and shared by the source tarball and every image build's context. (The content hash
/// is a different view: it records a submodule by the commit its gitlink pins.)
#[derive(Debug, Clone)]
pub(crate) struct TrackedTree {
    files: Vec<Tracked>,
    /// Submodules that are not checked out locally (an empty gitlink directory), root-relative.
    pub(crate) unpopulated_submodules: Vec<String>,
}

impl TrackedTree {
    /// List `root`'s tracked files (recursing into populated submodules).
    pub(crate) fn list(root: &Path) -> CliResult<Self> {
        let mut tree = TrackedTree { files: Vec::new(), unpopulated_submodules: Vec::new() };
        collect_tracked_recursive(root, "", &mut tree.files, &mut tree.unpopulated_submodules)?;
        Ok(tree)
    }
}

/// One file of the source the coordinator gets: its root-relative path and, for a symlink, the
/// link's text (as git stores it).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShippedFile {
    pub(crate) path: String,
    pub(crate) symlink_target: Option<String>,
}

/// What a path names in a [`SourceFiles`], following its symlinks as the coordinator's checkout
/// would.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Named {
    /// A file, at the path it resolves to.
    File(String),
    /// A directory holding shipped files, at the path it resolves to (`""` is the root).
    Dir(String),
    /// Nothing shipped is there.
    Missing,
    /// Following the symlink at `link` leaves the repository.
    Outside { link: String },
    /// The symlink at `link` is part of a loop.
    Loop { link: String },
}

/// The files the coordinator gets for a source mode ([`shipped_files`]): what an entrypoint must
/// name, what engine detection looks at, and what the memory check reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SourceFiles {
    /// Path -> the symlink's target, for a symlink.
    files: std::collections::BTreeMap<String, Option<String>>,
    /// Every directory holding a shipped file (the root excepted).
    dirs: std::collections::HashSet<String>,
    /// Whether submodules' files are left out (a git-mode clone has none).
    without_submodules: bool,
}

impl SourceFiles {
    pub(crate) fn new(
        files: impl IntoIterator<Item = ShippedFile>,
        without_submodules: bool,
    ) -> Self {
        let files: std::collections::BTreeMap<String, Option<String>> =
            files.into_iter().map(|f| (f.path, f.symlink_target)).collect();
        let dirs = files
            .keys()
            .flat_map(|path| path.match_indices('/').map(|(i, _)| path[..i].to_string()))
            .collect();
        SourceFiles { files, dirs, without_submodules }
    }

    /// A git-mode listing of plain files, for tests.
    #[cfg(test)]
    pub(crate) fn of(paths: &[&str]) -> Self {
        let files = paths.iter().map(|p| ShippedFile { path: p.to_string(), symlink_target: None });
        Self::new(files, true)
    }

    /// The root-relative paths, symlinks included, in order.
    pub(crate) fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    /// Which files these are, for a message saying a path is not among them.
    pub(crate) fn describe(&self) -> &'static str {
        if self.without_submodules {
            "its git-tracked files, not git-ignored ones, and none inside a submodule (a git-mode \
             source is a clone without them)"
        } else {
            "its git-tracked files, not git-ignored ones (populated submodules included)"
        }
    }

    /// Whether `path` names a file, through any symlinks.
    pub(crate) fn is_file(&self, path: &str) -> bool {
        matches!(self.resolve(path), Named::File(_))
    }

    /// What the root-relative, `/`-separated `path` names. A symlink is followed through its
    /// target text (relative to the link's directory, `..` included, as the kernel resolves it),
    /// so an entrypoint reached through a tracked symlink counts; one leaving the repository (an
    /// absolute target, or `..` past the root) or looping does not.
    pub(crate) fn resolve(&self, path: &str) -> Named {
        // Linux's own limit on symlinks followed in one lookup.
        const MAX_LINKS: usize = 40;
        let mut todo: std::collections::VecDeque<String> =
            path.split('/').map(str::to_string).collect();
        let mut done: Vec<String> = Vec::new();
        let mut links = 0;
        let mut last_link: Option<String> = None;
        while let Some(part) = todo.pop_front() {
            match part.as_str() {
                "" | "." => continue,
                ".." => {
                    if done.pop().is_none() {
                        return Named::Outside {
                            link: last_link.unwrap_or_else(|| path.to_string()),
                        };
                    }
                    continue;
                }
                _ => done.push(part),
            }
            let here = done.join("/");
            match self.files.get(&here) {
                Some(Some(target)) => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Named::Loop { link: here };
                    }
                    if target.starts_with('/') {
                        return Named::Outside { link: here };
                    }
                    done.pop();
                    for part in target.split('/').rev() {
                        todo.push_front(part.to_string());
                    }
                    last_link = Some(here);
                }
                // A file ends the path: anything after it but `.` or `/` names nothing.
                Some(None) if todo.iter().all(|p| p.is_empty() || p == ".") => {
                    return Named::File(here)
                }
                Some(None) => return Named::Missing,
                None if self.dirs.contains(&here) => {}
                None => return Named::Missing,
            }
        }
        if self.files.is_empty() {
            return Named::Missing;
        }
        Named::Dir(done.join("/"))
    }
}

/// The files the coordinator gets for `mode`: with `s3`, the staged tree (populated submodules
/// included; `tree` when already listed); with `git`, the commit's tracked files a clone checks
/// out (no submodules: the coordinator clones without them).
pub(crate) fn shipped_files(
    root: &Path,
    mode: SourceMode,
    tree: Option<&TrackedTree>,
) -> CliResult<SourceFiles> {
    Ok(match mode {
        SourceMode::S3 => {
            let listed;
            let tree = match tree {
                Some(tree) => tree,
                None => {
                    listed = TrackedTree::list(root)?;
                    &listed
                }
            };
            let files = tree.files.iter().map(|t| ShippedFile {
                path: t.rel.clone(),
                symlink_target: t.symlink_target.clone(),
            });
            SourceFiles::new(files, false)
        }
        SourceMode::Git => {
            let files = tracked_files(root)?
                .into_iter()
                .filter(|e| e.gitlink_sha.is_none())
                .map(|e| ShippedFile { path: e.path, symlink_target: e.symlink_target });
            SourceFiles::new(files, true)
        }
    })
}

/// Build a deterministic gzipped tarball of the workflow's tracked source tree, for S3 staging
/// (so private repos work): `tree`'s files at root-relative paths,
/// populated submodules' included. This handles private submodules for free: their bytes are
/// already in the local checkout (cloned once with the registrant's own credentials), so the
/// coordinator never needs to clone anything. Metadata is normalized (mtime/uid/gid zeroed, mode
/// reduced to 0644/0755) so identical source produces byte-identical archives; only the executable
/// bit is preserved. A tracked symlink is staged as a symlink, never as the file it points to.
pub(crate) fn tar_tracked_files(tree: &TrackedTree) -> CliResult<Vec<u8>> {
    tar_files(&mut read_all(&tree.files)?)
}

/// An image build's context, staged apart from the source: the tracked files under one directory.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContextTarball {
    /// The gzipped tar, paths relative to the context directory.
    pub(crate) bytes: Vec<u8>,
    /// [`biffix_workflow::version::tree_hash`] of the files (paths within the context, executable
    /// bits and symlinks included, as the tarball has them): what the build's key is made from,
    /// independent of how the tarball is compressed.
    pub(crate) tree_hash: String,
    /// The tarball's sha256 (hex), which the build checks before using it.
    pub(crate) sha256: String,
    /// The files in it (paths within the context), sorted.
    pub(crate) files: Vec<String>,
}

/// Tar the tracked files of `tree` under `context` (a directory relative to the root; `` for the
/// root itself), at paths relative to it, deterministically. `only` limits it to those files (each
/// must be tracked), for a context of known files such as a pixi environment's.
pub(crate) fn context_tarball(
    tree: &TrackedTree,
    context: &str,
    only: Option<&[&str]>,
) -> CliResult<ContextTarball> {
    let prefix = if context.is_empty() { String::new() } else { format!("{context}/") };
    let mut files: Vec<Tracked> = tree
        .files
        .iter()
        .filter_map(|t| {
            let within = t.rel.strip_prefix(&prefix)?;
            only.is_none_or(|names| names.contains(&within))
                .then(|| Tracked { rel: within.to_string(), ..t.clone() })
        })
        .collect();
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    if let Some(names) = only {
        for name in names {
            if !files.iter().any(|t| t.rel == *name) {
                return Err(CliError::InvalidInput(format!(
                    "{prefix}{name} is not a tracked file (only git-tracked files are built)"
                )));
            }
        }
    }
    if files.is_empty() {
        return Err(CliError::InvalidInput(format!(
            "the build context `{}` has no tracked files",
            context_label(context)
        )));
    }
    let mut contents = read_all(&files)?;
    let bytes = tar_files(&mut contents)?;
    let names = files.into_iter().map(|t| t.rel).collect();
    let tree_hash = biffix_workflow::version::tree_hash(contents);
    let sha256 = sha256_hex(&bytes);
    Ok(ContextTarball { bytes, tree_hash, sha256, files: names })
}

/// How a build context is shown: the empty context (the source root) as `.`.
pub(crate) fn context_label(context: &str) -> &str {
    if context.is_empty() {
        "."
    } else {
        context
    }
}

/// The sha256 (hex) of `bytes`: what a staged tarball is recorded, and checked, by.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Read each tracked file as the version sees it (its kind and bytes; a symlink's target text).
fn read_all(files: &[Tracked]) -> CliResult<Vec<TreeFile>> {
    files
        .iter()
        .map(|t| {
            let (kind, bytes) =
                biffix_workflow::version::read_tree_file(&t.abs, t.symlink_target.as_deref())
                    .map_err(|e| {
                        CliError::InvalidInput(format!(
                            "cannot read tracked file {}: {e}",
                            t.abs.display()
                        ))
                    })?;
            Ok(TreeFile { path: t.rel.clone(), kind, bytes })
        })
        .collect()
}

/// A deterministic gzipped tar of `files` (sorted in place): mtime/uid/gid zeroed and modes
/// reduced to 0644/0755 (a symlink is stored as a link to its target text), so identical files
/// make identical bytes.
fn tar_files(files: &mut [TreeFile]) -> CliResult<Vec<u8>> {
    // Deterministic member order: identical trees → byte-identical archives.
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(gz);
    for file in files.iter() {
        let rel_path = &file.path;
        let mut header = tar::Header::new_gnu();
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        let added = match file.kind {
            FileKind::Symlink => {
                let target = String::from_utf8_lossy(&file.bytes).into_owned();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                // `append_link` sets the path and the link name (long ones too) and the checksum.
                builder.append_link(&mut header, rel_path, target)
            }
            FileKind::Regular | FileKind::Executable => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(file.bytes.len() as u64);
                header.set_mode(if file.kind == FileKind::Executable { 0o755 } else { 0o644 });
                // `append_data` sets the (repo-root-relative) path on the header and computes its
                // checksum.
                builder.append_data(&mut header, rel_path, file.bytes.as_slice())
            }
        };
        added.map_err(|e| CliError::InvalidInput(format!("cannot add {rel_path} to tar: {e}")))?;
    }

    let gz =
        builder.into_inner().map_err(|e| CliError::InvalidInput(format!("finalizing tar: {e}")))?;
    gz.finish().map_err(|e| CliError::InvalidInput(format!("finalizing gzip: {e}")))
}

/// Walk `root`'s tracked files into `files`, recursing into populated submodules (their tracked
/// files are staged under the gitlink's path, including nested submodules). A gitlink whose
/// working tree isn't checked out locally is recorded in `unpopulated` rather than descended into.
fn collect_tracked_recursive(
    root: &Path,
    prefix: &str,
    files: &mut Vec<Tracked>,
    unpopulated: &mut Vec<String>,
) -> CliResult<()> {
    for entry in tracked_files(root)? {
        let rel_path =
            if prefix.is_empty() { entry.path.clone() } else { format!("{prefix}/{}", entry.path) };
        if entry.gitlink_sha.is_some() {
            let submodule_root = root.join(&entry.path);
            if is_checked_out_submodule(&submodule_root) {
                collect_tracked_recursive(&submodule_root, &rel_path, files, unpopulated)?;
            } else {
                unpopulated.push(rel_path);
            }
        } else {
            files.push(Tracked {
                rel: rel_path,
                abs: root.join(&entry.path),
                symlink_target: entry.symlink_target,
            });
        }
    }
    Ok(())
}

/// Whether a gitlink's directory is a checked-out repository of its own (a populated submodule, or
/// an embedded repository), so its own tracked files can be enumerated. Asking git whether `dir`
/// is inside a work tree is not enough: an unpopulated submodule's empty directory (the state a
/// plain `git clone` leaves) is inside the *parent's* work tree. A checked-out one has its own
/// `.git` (a `gitdir:` file for a submodule, a directory for an embedded repository).
fn is_checked_out_submodule(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Compute the deterministic content hash of the workflow's tracked source tree, using the shared
/// `biffix-workflow` normalization + hash so it equals what any other producer would compute. A
/// submodule is covered by the commit its gitlink pins, not by its files: a staged tarball matches
/// that only while every submodule is checked out at its pinned commit without edits, which
/// `register` requires before staging one.
pub(crate) fn content_hash(root: &Path) -> CliResult<String> {
    let tracked = tracked_files(root)?;
    let files = biffix_workflow::version::normalized_tree(root, &tracked).map_err(|e| {
        CliError::InvalidInput(format!("cannot read source files under {}: {e}", root.display()))
    })?;
    if files.is_empty() {
        return Err(CliError::InvalidInput(format!(
            "no git-tracked files under {}; nothing to register",
            root.display()
        )));
    }
    Ok(biffix_workflow::version::tree_hash(files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_git::git_ok;

    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| git_ok(dir, args);
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
    }

    /// The source tarball of `root` and its unpopulated submodules.
    fn tar_of(root: &Path) -> CliResult<(Vec<u8>, Vec<String>)> {
        let tree = TrackedTree::list(root)?;
        Ok((tar_tracked_files(&tree)?, tree.unpopulated_submodules))
    }

    /// A build context of `root`.
    fn ctx(root: &Path, context: &str, only: Option<&[&str]>) -> CliResult<ContextTarball> {
        context_tarball(&TrackedTree::list(root)?, context, only)
    }

    /// `base/sub` (one committed `rules.smk`) and `base/wf`, which vendors it at `lib/` and has a
    /// committed `Snakefile`.
    fn repo_with_submodule(base: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let sub = base.join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo(&sub);
        std::fs::write(sub.join("rules.smk"), b"# shared rules\n").unwrap();
        git_ok(&sub, &["add", "rules.smk"]);
        git_ok(&sub, &["commit", "-q", "-m", "sub"]);
        let wf = base.join("wf");
        std::fs::create_dir(&wf).unwrap();
        init_repo(&wf);
        std::fs::write(wf.join("Snakefile"), b"include: 'lib/rules.smk'\n").unwrap();
        git_ok(&wf, &["add", "Snakefile"]);
        git_ok(
            &wf,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "lib",
            ],
        );
        git_ok(&wf, &["commit", "-q", "-m", "add submodule"]);
        (wf, sub)
    }

    #[test]
    fn the_shipped_files_are_what_the_coordinator_gets() {
        // The staged tree includes a populated submodule's files; a clone has none of them. An
        // untracked or ignored file is in neither.
        let dir = tempfile::tempdir().unwrap();
        let (wf, _) = repo_with_submodule(dir.path());
        std::fs::write(wf.join(".gitignore"), b"ignored.smk\n").unwrap();
        std::fs::write(wf.join("ignored.smk"), b"").unwrap();
        std::fs::write(wf.join("untracked.smk"), b"").unwrap();
        let staged = shipped_files(&wf, SourceMode::S3, None).unwrap();
        assert_eq!(staged.paths(), [".gitmodules", "Snakefile", "lib/rules.smk"]);
        let listed = TrackedTree::list(&wf).unwrap();
        assert_eq!(shipped_files(&wf, SourceMode::S3, Some(&listed)).unwrap(), staged);
        let cloned = shipped_files(&wf, SourceMode::Git, None).unwrap();
        assert_eq!(cloned.paths(), [".gitmodules", "Snakefile"]);
        assert!(staged.is_file("lib/rules.smk") && !cloned.is_file("lib/rules.smk"));
        assert!(cloned.describe().contains("submodule"));
    }

    #[test]
    fn an_entrypoint_in_a_populated_submodule_is_shipped_by_s3_but_not_git() {
        use biffix_workflow::engines::Engine;
        use biffix_workflow::manifest::BiffixManifest;
        let dir = tempfile::tempdir().unwrap();
        let (wf, _) = repo_with_submodule(dir.path());
        let manifest = BiffixManifest {
            entrypoint: Some("lib/rules.smk".into()),
            ..BiffixManifest::default()
        };
        let resolve = |mode| {
            let files = shipped_files(&wf, mode, None).unwrap();
            crate::local::entrypoint::resolve_entrypoint(
                None,
                true,
                &manifest,
                Engine::Snakemake,
                &files,
            )
        };
        assert_eq!(resolve(SourceMode::S3).unwrap().path, "lib/rules.smk");
        let err = resolve(SourceMode::Git).unwrap_err().to_string();
        assert!(
            err.contains("not in the source biffix ships") && err.contains("submodule"),
            "{err}"
        );
    }

    #[test]
    fn a_path_is_resolved_through_tracked_symlinks_inside_the_repo() {
        let link = |path: &str, target: &str| ShippedFile {
            path: path.into(),
            symlink_target: Some(target.into()),
        };
        let file = |path: &str| ShippedFile { path: path.into(), symlink_target: None };
        let files = SourceFiles::new(
            [
                file("pipelines/v2/Snakefile"),
                file("pipelines/v2/rules/a.smk"),
                link("workflow", "pipelines/v2"),
                link("current", "workflow"),
                link("pipelines/v2/up", "../../pipelines/v2/Snakefile"),
                link("out", "../elsewhere"),
                link("abs", "/etc"),
                link("a", "b"),
                link("b", "a"),
                link("dangling", "nope"),
            ],
            true,
        );
        let at = |p: &str| Named::File(p.to_string());
        assert_eq!(files.resolve("workflow/Snakefile"), at("pipelines/v2/Snakefile"));
        assert_eq!(files.resolve("current/Snakefile"), at("pipelines/v2/Snakefile"));
        assert_eq!(files.resolve("workflow/up"), at("pipelines/v2/Snakefile"));
        assert_eq!(files.resolve("workflow/rules"), Named::Dir("pipelines/v2/rules".into()));
        assert_eq!(files.resolve(""), Named::Dir(String::new()));
        assert_eq!(files.resolve("workflow/Snakefile/x"), Named::Missing);
        assert_eq!(files.resolve("dangling"), Named::Missing);
        assert_eq!(files.resolve("workflow/nope"), Named::Missing);
        assert_eq!(files.resolve("out/x"), Named::Outside { link: "out".into() });
        assert_eq!(files.resolve("abs/passwd"), Named::Outside { link: "abs".into() });
        assert!(matches!(files.resolve("a/Snakefile"), Named::Loop { .. }));
        assert_eq!(SourceFiles::new([], true).resolve(""), Named::Missing);
    }

    #[test]
    fn a_plain_clone_s_unpopulated_submodule_is_reported_by_its_path() {
        // `git clone` without `--recurse-submodules` leaves `lib/` an empty directory, which is
        // inside the parent's work tree: it must be reported as `lib`, not recursed into as
        // `lib/./././…`.
        let dir = tempfile::tempdir().unwrap();
        let (wf, _) = repo_with_submodule(dir.path());
        let clone = dir.path().join("clone");
        git_ok(dir.path(), &["clone", "-q", wf.to_str().unwrap(), clone.to_str().unwrap()]);
        assert!(clone.join("lib").is_dir());
        let (tarball, unpopulated) = tar_of(&clone).unwrap();
        assert_eq!(unpopulated, ["lib"]);
        assert_eq!(read_tar(&tarball).keys().collect::<Vec<_>>(), [".gitmodules", "Snakefile"]);
        // Inside the empty directory, the parent's `./` gitlink names no file.
        assert!(tracked_files(&clone.join("lib")).unwrap().is_empty());
    }

    #[test]
    fn a_submodule_checked_out_elsewhere_or_edited_is_modified_but_its_untracked_files_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let (wf, _) = repo_with_submodule(dir.path());
        let info = resolve_git(&wf).unwrap();
        assert!(!info.dirty && info.modified_submodules.is_empty());
        std::fs::write(wf.join("lib/scratch.txt"), b"x").unwrap();
        let info = resolve_git(&wf).unwrap();
        assert!(!info.dirty && info.modified_submodules.is_empty());
        std::fs::write(wf.join("lib/rules.smk"), b"# edited\n").unwrap();
        let info = resolve_git(&wf).unwrap();
        assert!(info.dirty);
        assert_eq!(info.modified_submodules, ["lib"]);
        git_ok(&wf.join("lib"), &["checkout", "-q", "rules.smk"]);
        init_repo(&wf.join("lib"));
        git_ok(&wf.join("lib"), &["commit", "-q", "--allow-empty", "-m", "moved"]);
        let info = resolve_git(&wf).unwrap();
        assert!(info.dirty);
        assert_eq!(info.modified_submodules, ["lib"]);
        // Staging the bump makes the checkout the pinned commit again (still uncommitted).
        git_ok(&wf, &["add", "lib"]);
        let info = resolve_git(&wf).unwrap();
        assert!(info.dirty && info.modified_submodules.is_empty());
    }

    #[test]
    fn a_modified_submodule_is_named_relative_to_a_root_below_the_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let (_, sub) = repo_with_submodule(dir.path());
        let top = dir.path().join("top");
        std::fs::create_dir_all(top.join("pipe")).unwrap();
        init_repo(&top);
        std::fs::write(top.join("pipe/Snakefile"), b"include: 'lib/rules.smk'\n").unwrap();
        git_ok(&top, &["add", "pipe/Snakefile"]);
        git_ok(
            &top,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "pipe/lib",
            ],
        );
        git_ok(&top, &["commit", "-q", "-m", "add submodule"]);
        std::fs::write(top.join("pipe/lib/rules.smk"), b"# edited\n").unwrap();
        let info = resolve_git(&top.join("pipe")).unwrap();
        assert_eq!(info.prefix, "pipe/");
        assert_eq!(info.modified_submodules, ["lib"]);
        assert_eq!(resolve_git(&top).unwrap().modified_submodules, ["pipe/lib"]);
    }

    #[test]
    fn the_dirty_check_ignores_untracked_files_and_changes_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        std::fs::create_dir_all(root.join("wf")).unwrap();
        std::fs::write(root.join("wf/Snakefile"), b"rule all:\n").unwrap();
        std::fs::write(root.join("README.md"), b"docs\n").unwrap();
        git_ok(root, &["add", "-A"]);
        git_ok(root, &["commit", "-q", "-m", "c"]);
        let wf = root.join("wf");
        let info = resolve_git(&wf).unwrap();
        assert_eq!(info.prefix, "wf/");
        assert_eq!(resolve_git(root).unwrap().prefix, "");
        std::fs::write(root.join("scratch.txt"), b"x").unwrap();
        std::fs::write(wf.join("notes.txt"), b"x").unwrap();
        std::fs::write(root.join("README.md"), b"edited\n").unwrap();
        assert!(!resolve_git(&wf).unwrap().dirty);
        assert!(resolve_git(root).unwrap().dirty);
        std::fs::write(wf.join("Snakefile"), b"rule all:\n  input: []\n").unwrap();
        assert!(resolve_git(&wf).unwrap().dirty);
    }

    #[test]
    fn status_records_are_read_by_kind() {
        let recs = |r: &[&str]| {
            parse_status(&r.iter().map(std::string::ToString::to_string).collect::<Vec<_>>())
        };
        let sha = "0".repeat(40);
        assert_eq!(recs(&[]), Status::default());
        let renamed = format!("2 R. N... 100644 100644 100644 {sha} {sha} R100 new name");
        let got = recs(&[&renamed, "old name"]);
        assert!(got.dirty && got.modified_submodules.is_empty());
        let sub = format!("1 .M S..U 160000 160000 160000 {sha} {sha} lib dir");
        assert_eq!(recs(&[&sub]), Status::default());
        let sub = format!("1 .M SC.. 160000 160000 160000 {sha} {sha} lib dir");
        assert_eq!(recs(&[&sub]).modified_submodules, ["lib dir"]);
        assert!(recs(&["u UU N... 100644 100644 100644 100644 a b c x"]).dirty);
    }

    #[test]
    fn paths_git_would_quote_are_listed_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let names = ["é.txt", "a\"b", "back\\slash", "tab\there", "trailing "];
        for name in names {
            std::fs::write(root.join(name), name.as_bytes()).unwrap();
        }
        git_ok(root, &["add", "-A"]);
        let mut listed: Vec<String> =
            tracked_files(root).unwrap().into_iter().map(|e| e.path).collect();
        listed.sort();
        let mut expected: Vec<String> =
            names.iter().map(std::string::ToString::to_string).collect();
        expected.sort();
        assert_eq!(listed, expected);
        content_hash(root).expect("every listed path is readable");
        let (tarball, _) = tar_of(root).unwrap();
        assert_eq!(read_tar(&tarball).len(), names.len());
    }

    #[test]
    fn credentials_are_stripped_from_an_origin() {
        // Any scheme's password goes; a user without one is kept.
        for (with, without) in [
            (
                "ssh://deploy:hunter2@git.example.org/g/r.git",
                "ssh://deploy@git.example.org/g/r.git",
            ),
            ("git://u:p@git.example.org/g/r.git", "git://u@git.example.org/g/r.git"),
        ] {
            assert_eq!(
                strip_userinfo(with),
                (without.to_string(), Some("git.example.org".to_string()))
            );
        }
        assert_eq!(
            strip_userinfo("https://x-access-token:ghp_secret@github.com/org/repo.git"),
            ("https://github.com/org/repo.git".to_string(), Some("github.com".to_string()))
        );
        assert_eq!(
            strip_userinfo("https://glpat-token@gitlab.example.org/g/r"),
            ("https://gitlab.example.org/g/r".to_string(), Some("gitlab.example.org".to_string()))
        );
        for plain in [
            "https://github.com/org/repo.git",
            "git@github.com:org/repo.git",
            "ssh://git@github.com/org/repo.git",
        ] {
            assert_eq!(strip_userinfo(plain), (plain.to_string(), None));
        }
    }

    #[test]
    fn resolves_sha_and_hashes_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        std::fs::write(root.join("Snakefile"), b"rule all:\n").unwrap();
        git_ok(root, &["add", "Snakefile"]);
        git_ok(root, &["commit", "-q", "-m", "init"]);

        let info = resolve_git(root).unwrap();
        assert_eq!(info.sha.len(), 40);
        assert!(!info.dirty);

        let h1 = content_hash(root).unwrap();
        // Deterministic: the same tree hashes identically.
        assert_eq!(h1, content_hash(root).unwrap());

        // A new tracked file changes the hash; an untracked one does not (ls-files ignores it).
        std::fs::write(root.join("extra.txt"), b"x").unwrap();
        assert_eq!(h1, content_hash(root).unwrap(), "untracked file must not affect the hash");
        git_ok(root, &["add", "extra.txt"]);
        assert_ne!(h1, content_hash(root).unwrap(), "a new tracked file must change the hash");

        // The tree is now dirty (staged change uncommitted).
        assert!(resolve_git(root).unwrap().dirty);
    }

    #[test]
    fn hashes_a_submodule_gitlink_instead_of_crashing() {
        // Regression: a workflow repo with a submodule (e.g. vendor/minibwa) used to die with
        // "cannot read source files under .: Is a directory (os error 21)" because ls-files listed
        // the gitlink and normalization read it as a file. Now the recorded commit SHA is hashed.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let run = |args: &[&str]| git_ok(root, args);
        std::fs::write(root.join("Snakefile"), b"rule all:\n").unwrap();
        run(&["add", "Snakefile"]);
        // Inject a submodule gitlink into the index (mode 160000) with no real submodule needed.
        let sha_a = "1".repeat(40);
        run(&["update-index", "--add", "--cacheinfo", &format!("160000,{sha_a},vendor/minibwa")]);

        // ls-files -s flags it as a gitlink carrying its recorded SHA.
        let entries = tracked_files(root).unwrap();
        let gl = entries.iter().find(|e| e.path == "vendor/minibwa").unwrap();
        assert_eq!(gl.gitlink_sha.as_deref(), Some(sha_a.as_str()));

        // Hashing succeeds (no "Is a directory") and is deterministic.
        let h1 = content_hash(root).expect("a submodule gitlink must not break content hashing");
        assert_eq!(h1, content_hash(root).unwrap());

        // A submodule bump (different pinned SHA) changes the version hash — provenance is covered.
        let sha_b = "2".repeat(40);
        run(&["update-index", "--cacheinfo", &format!("160000,{sha_b},vendor/minibwa")]);
        assert_ne!(h1, content_hash(root).unwrap(), "a submodule bump must change the hash");
    }

    /// Read a gzipped tarball back into a `{path: (bytes, mode)}` map for assertions.
    fn read_tar(gz: &[u8]) -> std::collections::BTreeMap<String, (Vec<u8>, u32)> {
        let dec = flate2::read::GzDecoder::new(gz);
        let mut archive = tar::Archive::new(dec);
        let mut out = std::collections::BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mode = entry.header().mode().unwrap();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
            out.insert(path, (buf, mode));
        }
        out
    }

    #[cfg(unix)]
    #[test]
    fn tar_is_deterministic_and_stages_siblings_with_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let run = |args: &[&str]| git_ok(root, args);
        std::fs::write(root.join("Snakefile"), b"rule all:\n").unwrap();
        std::fs::create_dir(root.join("config")).unwrap();
        // A sibling of workflow/ read via plain open() — the file that must be staged (the whole
        // point of the full-tree deployment for the coordinator's no-shared-fs workers).
        std::fs::write(root.join("config/samples.yaml"), b"a: 1\n").unwrap();
        std::fs::write(root.join("run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        run(&["add", "Snakefile", "config/samples.yaml", "run.sh"]);
        // Mark the script executable via the git index (portable across the test host's umask).
        run(&["update-index", "--chmod=+x", "run.sh"]);
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        run(&["commit", "-q", "-m", "init"]);

        let (a, skipped_a) = tar_of(root).unwrap();
        let (b, _skipped_b) = tar_of(root).unwrap();
        assert_eq!(a, b, "identical tree must produce a byte-identical archive");
        assert!(skipped_a.is_empty(), "no submodules to skip here");

        let entries = read_tar(&a);
        assert_eq!(entries.get("Snakefile").map(|(c, _)| c.clone()), Some(b"rule all:\n".to_vec()));
        assert_eq!(
            entries.get("config/samples.yaml").map(|(c, _)| c.clone()),
            Some(b"a: 1\n".to_vec()),
            "a sibling of the workflow dir must be staged"
        );
        assert!(
            entries.get("run.sh").unwrap().1 & 0o111 != 0,
            "the executable bit must be preserved"
        );
        assert_eq!(
            entries.get("Snakefile").unwrap().1 & 0o111,
            0,
            "a non-executable file must stay non-executable"
        );
    }

    #[test]
    fn tar_reports_an_unpopulated_submodule_and_stages_nothing_for_it() {
        // A gitlink whose working tree isn't checked out locally has no bytes to stage; it is
        // reported (so the CLI warns) and contributes nothing to the tar.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        let run = |args: &[&str]| git_ok(root, args);
        std::fs::write(root.join("Snakefile"), b"rule all:\n").unwrap();
        run(&["add", "Snakefile"]);
        run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},vendor/minibwa", "1".repeat(40)),
        ]);

        let (tarball, unpopulated) = tar_of(root).unwrap();
        assert_eq!(unpopulated, vec!["vendor/minibwa".to_string()]);
        let entries = read_tar(&tarball);
        assert!(entries.contains_key("Snakefile"));
        assert!(
            !entries.keys().any(|k| k.starts_with("vendor/minibwa")),
            "an unpopulated submodule must not appear in the tar"
        );
    }

    #[test]
    fn tar_recurses_a_populated_submodule() {
        // A checked-out submodule's tracked source IS staged, at its gitlink path — the
        // private-repo win: the bytes are already local, so no clone (or credential) is needed at
        // run time.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // A standalone repo that will become the submodule.
        let sub_src = base.join("sub_src");
        std::fs::create_dir(&sub_src).unwrap();
        init_repo(&sub_src);
        std::fs::write(sub_src.join("rules.smk"), b"# shared rules\n").unwrap();
        let sub_run = |args: &[&str]| git_ok(&sub_src, args);
        sub_run(&["add", "rules.smk"]);
        sub_run(&["commit", "-q", "-m", "sub"]);

        // The outer workflow repo, which vendors sub_src at `lib/`.
        let outer = base.join("wf");
        std::fs::create_dir(&outer).unwrap();
        init_repo(&outer);
        std::fs::write(outer.join("Snakefile"), b"include: 'lib/rules.smk'\n").unwrap();
        let outer_run = |args: &[&str]| git_ok(&outer, args);
        outer_run(&["add", "Snakefile"]);
        // Local-path submodules need protocol.file.allow=always on modern git.
        outer_run(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub_src.to_str().unwrap(),
            "lib",
        ]);
        outer_run(&["commit", "-q", "-m", "add submodule"]);

        let (tarball, unpopulated) = tar_of(&outer).unwrap();
        assert!(
            unpopulated.is_empty(),
            "a checked-out submodule is not unpopulated: {unpopulated:?}"
        );
        let entries = read_tar(&tarball);
        assert!(entries.contains_key("Snakefile"));
        assert!(
            entries.contains_key("lib/rules.smk"),
            "the submodule's tracked source must be staged at its gitlink path"
        );
        assert_eq!(
            entries.get("lib/rules.smk").map(|(c, _)| c.clone()),
            Some(b"# shared rules\n".to_vec())
        );
    }

    fn commit_all(dir: &Path) {
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "c"]);
    }

    #[test]
    fn a_build_context_is_its_tracked_files_hashed_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        std::fs::create_dir_all(root.join("docker/align")).unwrap();
        std::fs::write(root.join("docker/align/Dockerfile"), b"FROM x\n").unwrap();
        std::fs::write(root.join("docker/align/tool.sh"), b"echo\n").unwrap();
        std::fs::write(root.join("Snakefile"), b"rule all:\n").unwrap();
        commit_all(root);

        let first = ctx(root, "docker/align", None).unwrap();
        assert_eq!(first.files, ["Dockerfile", "tool.sh"]);
        assert_eq!(ctx(root, "docker/align", None).unwrap(), first);
        // A file outside the context, and an untracked one inside it, do not change it.
        std::fs::write(root.join("Snakefile"), b"rule all:\n  input: []\n").unwrap();
        commit_all(root);
        std::fs::write(root.join("docker/align/untracked.txt"), b"y").unwrap();
        assert_eq!(ctx(root, "docker/align", None).unwrap(), first);
        // Once tracked, it is part of the context (its content as it is on disk).
        commit_all(root);
        std::fs::write(root.join("docker/align/untracked.txt"), b"z").unwrap();
        let later = ctx(root, "docker/align", None).unwrap();
        assert_eq!(later.files, ["Dockerfile", "tool.sh", "untracked.txt"]);
        let only = ctx(root, "docker/align", Some(&["Dockerfile", "tool.sh"])).unwrap();
        assert_eq!(only.tree_hash, first.tree_hash);
        assert_eq!(only.bytes, first.bytes);
        // A file's content changes the hash.
        std::fs::write(root.join("docker/align/tool.sh"), b"echo 2\n").unwrap();
        let changed = ctx(root, "docker/align", None).unwrap();
        assert_ne!(changed.tree_hash, later.tree_hash);
        // The root context holds everything tracked; a named file must be tracked.
        assert!(ctx(root, "", None).unwrap().files.contains(&"Snakefile".to_string()));
        let err = ctx(root, "docker/align", Some(&["pixi.lock"])).unwrap_err();
        assert!(err.to_string().contains("docker/align/pixi.lock is not a tracked file"), "{err}");
        assert!(ctx(root, "nope", None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_tracked_symlink_is_hashed_and_staged_as_a_link() {
        // `envs -> shared/envs` (a directory: reading through it fails with `Is a directory`) and
        // `hosts -> <a file outside the repo>` (reading through it stages this machine's bytes).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();
        let abs = outside.path().join("hosts");
        std::fs::write(&abs, b"machine-local\n").unwrap();
        init_repo(root);
        std::fs::create_dir_all(root.join("shared/envs")).unwrap();
        std::fs::write(root.join("shared/envs/bwa.yaml"), b"deps: [bwa]\n").unwrap();
        std::fs::write(root.join("Snakefile"), b"rule all:\n").unwrap();
        std::os::unix::fs::symlink("shared/envs", root.join("envs")).unwrap();
        std::os::unix::fs::symlink(&abs, root.join("hosts")).unwrap();
        commit_all(root);

        let entries = tracked_files(root).unwrap();
        let link =
            |p: &str| entries.iter().find(|e| e.path == p).and_then(|e| e.symlink_target.clone());
        assert_eq!(link("envs").as_deref(), Some("shared/envs"));
        assert_eq!(link("hosts"), Some(abs.to_string_lossy().into_owned()));
        assert_eq!(link("Snakefile"), None);

        // Hashing succeeds, and what the outside file holds does not change it.
        let h1 = content_hash(root).expect("a tracked symlink must not break content hashing");
        std::fs::write(&abs, b"other bytes\n").unwrap();
        assert_eq!(h1, content_hash(root).unwrap());

        // The tarball holds the links themselves, not what they point to.
        let (tarball, _) = tar_of(root).unwrap();
        let dec = flate2::read::GzDecoder::new(tarball.as_slice());
        let mut archive = tar::Archive::new(dec);
        let mut links = std::collections::BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            if entry.header().entry_type() == tar::EntryType::Symlink {
                links.insert(
                    entry.path().unwrap().to_string_lossy().into_owned(),
                    entry.link_name().unwrap().unwrap().to_string_lossy().into_owned(),
                );
            }
        }
        assert_eq!(links.get("envs").map(String::as_str), Some("shared/envs"));
        assert_eq!(links.get("hosts"), Some(&abs.to_string_lossy().into_owned()));
        let entries = read_tar(&tarball);
        assert_eq!(
            entries.get("shared/envs/bwa.yaml").map(|(c, _)| c.clone()),
            Some(b"deps: [bwa]\n".to_vec())
        );
        // A build context containing a link stages it as a link too.
        let ctx = ctx(root, "", Some(&["envs"])).unwrap();
        assert_eq!(ctx.files, ["envs"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_changes_a_context_s_tree_hash() {
        // The tarball keeps the bit, so a `chmod +x` fix is a new build, not the cached image.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        std::fs::write(root.join("Dockerfile"), b"FROM x\nCOPY run.sh /\n").unwrap();
        std::fs::write(root.join("run.sh"), b"#!/bin/sh\n").unwrap();
        let chmod = |m: u32| {
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(m))
                .unwrap();
        };
        chmod(0o644);
        commit_all(root);
        let plain = ctx(root, "", None).unwrap();
        chmod(0o755);
        let exec = ctx(root, "", None).unwrap();
        assert_ne!(plain.tree_hash, exec.tree_hash);
        let exec_source = content_hash(root).unwrap();
        chmod(0o644);
        assert_ne!(exec_source, content_hash(root).unwrap());
    }
}
