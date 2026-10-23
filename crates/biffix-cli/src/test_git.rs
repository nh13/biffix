//! The unit tests' `git`: every test that builds a throwaway repository runs git through here.
//!
//! A test run can inherit a `GIT_*` environment that points git at another repository: `git
//! rebase --exec` exports `GIT_DIR`, and with it set `git -C <tempdir> init` reinitializes (and
//! `git config` rewrites) the repository being rebased, not the temporary one. So the commands built
//! here drop every inherited `GIT_*` variable, and the first one also drops them from this test
//! process, so the CLI code under test (which runs git itself) reads the temporary repository too.
//!
//! `tests/it/common` has a copy for the integration tests, which cannot reach this module: keep the
//! two alike.

use std::path::Path;
use std::process::Command;
use std::sync::Once;

/// `git -C <dir>`, with no inherited `GIT_*` environment.
pub(crate) fn git(dir: &Path) -> Command {
    static SCRUB: Once = Once::new();
    SCRUB.call_once(|| {
        for key in git_env() {
            std::env::remove_var(key);
        }
    });
    let mut cmd = Command::new("git");
    for key in git_env() {
        cmd.env_remove(key);
    }
    cmd.arg("-C").arg(dir);
    cmd
}

/// Run `git -C <dir> <args>`, asserting it succeeds.
pub(crate) fn git_ok(dir: &Path, args: &[&str]) {
    let out = git(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// The names of this process's `GIT_*` environment variables.
fn git_env() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("GIT_"))
        .collect()
}
