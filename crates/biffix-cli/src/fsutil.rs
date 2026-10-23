//! Crash- and race-safe writes for the CLI's two state files (`config.toml`, `credentials.json`),
//! and for `biffix.yaml` when `inspect --write` edits it.
//!
//! [`write_atomic`] writes a sibling temp file and renames it over the target, so a reader (or a
//! crash) only ever sees the old or the new complete file. [`FileLock`] is an advisory lock on a
//! sibling `.lock` file, held across a read-modify-write so two `biffix` processes updating
//! different profiles cannot drop each other's update (the last rename would otherwise win).

use std::fs::File;
use std::path::{Path, PathBuf};

/// An exclusive advisory lock, released on drop (the OS releases it when the file is closed).
pub(crate) struct FileLock {
    _file: File,
}

impl FileLock {
    /// Block until the exclusive lock on `<target>.lock` is held. The lock file is created (owner
    /// only) if missing and never removed: removing it would let a waiter lock a now-unlinked
    /// inode while a newcomer locks a fresh one.
    ///
    /// Only ever held around synchronous file I/O (never across an `.await`), so two tasks in one
    /// process cannot deadlock a single-threaded runtime on it.
    pub(crate) fn acquire(target: &Path) -> std::io::Result<Self> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(lock_path(target))?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

fn lock_path(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.lock"))
}

/// Replace `path` with `bytes` atomically: write a unique sibling temp file (same directory, so the
/// rename stays on one filesystem), then `rename` it into place. With `owner_only` the temp file is
/// **created** mode 0600 and its mode re-verified before the rename, failing closed — a secret is
/// never on disk readable by others, even briefly. A failure removes only the temp file, so a bad
/// write never destroys the existing file. A `path` that is a symlink (a dotfiles manager's, say)
/// is written through: the file it points to is replaced, and the link kept.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], owner_only: bool) -> std::io::Result<()> {
    let resolved = std::fs::read_link(path).ok().and_then(|_| path.canonicalize().ok());
    let path = resolved.as_deref().unwrap_or(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(path);
    let result = write_new(&tmp, bytes, owner_only)
        .and_then(|()| if owner_only { verify_owner_only(&tmp) } else { Ok(()) })
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A unique sibling temp path; the pid + random nonce avoids a collision between concurrent writes.
fn temp_sibling(path: &Path) -> PathBuf {
    let nonce: u64 = rand::random();
    let stem = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{stem}.tmp-{}-{nonce:016x}", std::process::id()))
}

/// Create `path` fresh (`create_new`) and write `bytes`; owner-only from the outset on unix.
fn write_new(path: &Path, bytes: &[u8], owner_only: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if owner_only {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = owner_only;
    let mut f = options.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// The error [`write_atomic`] fails closed with when it cannot make the file owner-only, told
/// apart from an ordinary I/O failure (an unwritable directory is also `PermissionDenied`) by
/// [`is_not_owner_only`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct NotOwnerOnly(String);

/// Whether `e` is [`write_atomic`] refusing to write a file it cannot make owner-only.
pub(crate) fn is_not_owner_only(e: &std::io::Error) -> bool {
    matches!(e.get_ref(), Some(inner) if inner.is::<NotOwnerOnly>())
}

/// Set and re-check mode 0600, failing closed if a filesystem silently ignores it.
#[cfg(unix)]
fn verify_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            NotOwnerOnly(format!("stored file has mode {mode:o}, not 0600")),
        ));
    }
    Ok(())
}

/// Non-unix platforms are not supported; refuse rather than store a secret world-readable.
#[cfg(not(unix))]
fn verify_owner_only(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        NotOwnerOnly("owner-only permissions are only enforced on unix".into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_only_the_target_and_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.toml");
        write_atomic(&path, b"one", false).unwrap();
        {
            let _lock = FileLock::acquire(&path).unwrap();
            write_atomic(&path, b"two", true).unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec![".f.toml.lock".to_string(), "f.toml".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_written_through_its_link() {
        // A dotfiles manager links config.toml into its own tree; the link must keep working.
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("dotfiles");
        std::fs::create_dir(&managed).unwrap();
        std::fs::write(managed.join("config.toml"), "one").unwrap();
        let path = dir.path().join("config.toml");
        std::os::unix::fs::symlink(managed.join("config.toml"), &path).unwrap();
        write_atomic(&path, b"two", false).unwrap();
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(managed.join("config.toml")).unwrap(), b"two");
    }

    #[test]
    fn the_lock_excludes_a_second_holder_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let held = FileLock::acquire(&path).unwrap();
        let other = File::options().read(true).write(true).open(lock_path(&path)).unwrap();
        assert!(other.try_lock().is_err(), "a second holder must be excluded");
        drop(held);
        // Released on drop, but not always at once in a test binary: a lock belongs to the open
        // file, which a child another test forks at that moment (to run git) shares until it
        // execs. So released within a bound, never "not released".
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while other.try_lock().is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "released on drop (still held 5 s later)"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
