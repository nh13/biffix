//! `biffix logout` and `whoami`, run as the binary against a temporary home: where the profile's
//! state lives, and which failure each one reports.

use std::path::{Path, PathBuf};

use crate::common::{biffix, log_in};

/// The config directory the docs name: `~/.config/biffix` (macOS included).
fn config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("biffix")
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
fn terminal_output_is_filtered_but_json_values_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home-\u{1b}[2J\u{85}\u{7}");
    std::fs::create_dir_all(&home).unwrap();
    let profile = "test-\u{1b}[2J\u{85}\u{7}";
    let run = |format| {
        biffix(&home).args(["--profile", profile, "--output", format, "logout"]).output().unwrap()
    };
    let out = run("human");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"logged out of test-[2J\n");
    assert!(out.stderr.is_empty());
    let out = run("json");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        serde_json::json!({"status": "logged_out", "profile": profile})
    );
    assert!(out.stderr.is_empty());

    // A directory in place of config.toml takes the config-read error path (exit 1), whose
    // message includes its path. Exercise the real top-level stderr printer in both modes.
    std::fs::create_dir_all(config_dir(&home).join("config.toml")).unwrap();
    let error = |format| biffix(&home).args(["--output", format, "logout"]).output().unwrap();
    let json = error("json");
    assert_eq!(json.status.code(), Some(1));
    assert!(json.stdout.is_empty());
    let doc: serde_json::Value = serde_json::from_slice(&json.stderr).unwrap();
    let message = doc["message"].as_str().unwrap();
    assert!(message.contains(home.to_str().unwrap()), "{message:?}");
    let human = error("human");
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stdout.is_empty());
    let safe_message: String = message.chars().filter(|c| !c.is_control()).collect();
    assert_eq!(String::from_utf8(human.stderr).unwrap(), format!("error: {safe_message}\n"));
    for bytes in [&json.stderr, &json.stdout, &out.stdout] {
        assert!(String::from_utf8_lossy(bytes).chars().all(|c| !c.is_control() || c == '\n'));
    }
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
