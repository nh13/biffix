//! `biffix logout` and `whoami`, run as the binary against a temporary home: where the profile's
//! state lives, and which failure each one reports.

use std::path::{Path, PathBuf};

use crate::common::biffix;

/// The config directory the docs name: `~/.config/biffix` (macOS included).
fn config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("biffix")
}

/// A credential for `base` under `profile`, unexpired, in `home`'s credential store.
fn log_in(home: &Path, profile: &str, base: &str) {
    let dir = config_dir(home);
    std::fs::create_dir_all(&dir).unwrap();
    let creds = serde_json::json!({"profiles": {profile: {
        "access_token": "tok", "expires_at_unix": u64::MAX / 2, "subject": "user-1",
        "email": "bio@lab.example.org", "api_base_url": base,
    }}});
    std::fs::write(dir.join("credentials.json"), creds.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("credentials.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}

/// A deployment's `/auth/config`, advertising a CLI client.
async fn auth_config_stub() -> String {
    crate::common::serve(axum::Router::new().route(
        "/auth/config",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "authorization_endpoint": "http://127.0.0.1:9/oauth2/authorize",
                "token_endpoint": "http://127.0.0.1:9/oauth2/token",
                "client_id": "web", "redirect_uri": "http://x/cb",
                "logout_endpoint": "http://x/logout", "logout_uri": "http://x/out",
                "scopes": ["openid"], "cli_client_id": "cli-1",
                "cli_redirect_uris": ["http://localhost:8976/callback"],
            }))
        }),
    ))
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn login_logs_its_discovery_under_verbose() {
    let base = auth_config_stub().await;
    let home = tempfile::tempdir().unwrap();
    let mut cmd = biffix(home.path());
    cmd.args(["--verbose", "--allow-insecure", "--url", &base, "login", "--no-browser"])
        .stdin(std::process::Stdio::null());
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    // Nothing pasted, so the login itself fails; its discovery was logged first.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("[biffix] GET {base}/auth/config → 200 OK (attempt 1)")),
        "{stderr}"
    );
}

#[test]
fn logout_clears_the_profile_in_the_documented_config_dir() {
    let home = tempfile::tempdir().unwrap();
    log_in(home.path(), "default", "https://a.example/api");
    let out = biffix(home.path()).args(["--output", "json", "logout"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // The only profile is gone, and with it the file.
    assert!(!config_dir(home.path()).join("credentials.json").exists());
}

/// An unreadable config only matters to `logout` when nothing else names the profile, and then
/// its failure keeps its own kind: a config that does not parse is invalid input (exit 6), one
/// that cannot be read is not (exit 1).
#[test]
fn logout_names_the_profile_without_the_config_or_keeps_its_failure() {
    let home = tempfile::tempdir().unwrap();
    log_in(home.path(), "staging", "https://a.example/api");
    let config = config_dir(home.path()).join("config.toml");
    std::fs::write(&config, "not = [toml").unwrap();
    let logout = |args: &[&str]| biffix(home.path()).args(args).output().unwrap();

    let out = logout(&["logout"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(6), "{stderr}");
    assert!(stderr.contains("pass --profile"), "{stderr}");

    let out = logout(&["--profile", "staging", "logout"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!config_dir(home.path()).join("credentials.json").exists());

    // A config that cannot be read at all (a directory where the file goes).
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    let out = logout(&["logout"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cannot read config"), "{stderr}");
}

/// `whoami` with no credential is `AuthRequired` (exit 3) naming `biffix login`, before any
/// network call (the URL here reaches nothing).
#[test]
fn whoami_without_a_login_is_auth_required() {
    let home = tempfile::tempdir().unwrap();
    let out = biffix(home.path())
        .args(["--url", "https://unreachable.invalid/api", "whoami"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("biffix login"), "{stderr}");
}

/// A token the API accepts for a role too low to read the status report is still a session:
/// `whoami` succeeds and names the identity.
#[tokio::test(flavor = "multi_thread")]
async fn whoami_with_a_limited_role_is_still_signed_in() {
    let base = crate::common::serve(axum::Router::new().route(
        "/status",
        axum::routing::get(|| async { (axum::http::StatusCode::FORBIDDEN, "no ViewRuns") }),
    ))
    .await;
    let home = tempfile::tempdir().unwrap();
    log_in(home.path(), "default", &base);
    let mut cmd = biffix(home.path());
    cmd.args(["--allow-insecure", "--url", &base, "--output", "json", "whoami"]);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doc,
        serde_json::json!({"identity": "bio@lab.example.org", "subject": "user-1", "profile": "default"})
    );
}
