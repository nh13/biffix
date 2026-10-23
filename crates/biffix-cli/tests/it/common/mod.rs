//! Helpers shared by the integration tests: serve a tiny local stub and point a client at it; run
//! git with no inherited `GIT_*` environment (a copy of the unit tests' `src/test_git.rs`, which a
//! separate crate cannot reach: keep the two alike); build a git checkout; and run the `biffix`
//! binary in a temporary home.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use biffix_cli::api::ApiClient;
use biffix_cli::secret::Secret;

/// Held (shared) while a stub binds its port. A test that needs a port nothing listens on holds it
/// exclusively, so no stub of the same test binary is handed that port meanwhile.
pub(crate) static BIND_LOCK: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// Bind a free loopback port, holding `BIND_LOCK` (shared) only while binding.
pub(crate) async fn bind() -> tokio::net::TcpListener {
    let _binding = BIND_LOCK.read().await;
    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
}

/// Serve `app` on a free loopback port; returns its base URL.
pub(crate) async fn serve(app: axum::Router) -> String {
    let listener = bind().await;
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A client for `base` with a fixed bearer token (no refresh).
pub(crate) fn static_client(base: &str) -> ApiClient {
    ApiClient::new(reqwest::Client::new(), base, Secret::new("tok".into()), false)
}

/// The smallest run summary a `/runs` page row deserializes from.
pub(crate) fn minimal_run(i: usize) -> Value {
    json!({
        "run_id": format!("run-{i:04}"),
        "state": "succeeded",
        "tasks_observed": 0, "tasks_succeeded": 0, "tasks_running": 0,
        "tasks_failed": 0, "tasks_cached": 0,
    })
}

/// `git -C <dir>`, with no inherited `GIT_*` environment.
///
/// A test run can inherit a `GIT_*` environment that points git at another repository: `git rebase
/// --exec` exports `GIT_DIR`, and with it set `git -C <tempdir> init` reinitializes (and `git
/// config` rewrites) the repository being rebased, not the temporary one. So every git a test runs
/// goes through here, and the first call also drops those variables from this test process, so the
/// CLI code `e2e` calls in process (which runs git itself) reads the temporary repository too.
pub(crate) fn git(dir: &std::path::Path) -> std::process::Command {
    static SCRUB: std::sync::Once = std::sync::Once::new();
    SCRUB.call_once(|| {
        for key in git_env() {
            std::env::remove_var(key);
        }
    });
    let mut cmd = std::process::Command::new("git");
    without_git_env(&mut cmd).arg("-C").arg(dir);
    cmd
}

/// Run `git -C <dir> <args>`, asserting it succeeds.
pub(crate) fn git_ok(dir: &std::path::Path, args: &[&str]) {
    let out = git(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// A git checkout of `files`, added but not committed (the commands read the tracked tree).
pub(crate) fn checkout(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git_ok(dir.path(), &["init", "-q"]);
    for (path, text) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git_ok(dir.path(), &["add", "."]);
    dir
}

/// The stderr of `out`, asserting it succeeded (with that stderr as the message).
#[track_caller]
pub(crate) fn succeeded(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    stderr
}

/// `cmd` with every inherited `GIT_*` variable removed (for the `biffix` binary, which runs git).
pub(crate) fn without_git_env(cmd: &mut std::process::Command) -> &mut std::process::Command {
    for key in git_env() {
        cmd.env_remove(key);
    }
    cmd
}

/// The `biffix` binary with `home` as its home directory, its config in `home/.config/biffix`, and
/// none of the user's own environment that would point it elsewhere: no `XDG_CONFIG_HOME`, no
/// `GIT_*`, and no `BIFFIX_*` (which `e2e` sets while it runs in this same test binary).
pub(crate) fn biffix(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_biffix"));
    without_git_env(&mut cmd).env("HOME", home).env_remove("XDG_CONFIG_HOME");
    for var in ["BIFFIX_CONFIG_DIR", "BIFFIX_API_URL", "BIFFIX_ALLOW_INSECURE", "BIFFIX_PROFILE"] {
        cmd.env_remove(var);
    }
    cmd
}

/// The names of this process's `GIT_*` environment variables.
fn git_env() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("GIT_"))
        .collect()
}
