//! `biffix health`: probe a deployment's liveness and readiness, and — when the profile already
//! holds a credential — its per-component service status.
//!
//! This is the scriptable "did the deploy come up?" primitive. The liveness/readiness probes are
//! **unauthenticated** (`/healthz`, `/readyz`), so a fresh deploy can be verified with no login;
//! the authenticated `/status` detail is folded in opportunistically and its absence is never a
//! health failure. The command exits `0` only when both probes are OK, so CI and the deploy script
//! can gate on `biffix health`.

use std::time::Duration;

use biffix_api::StatusReport;

use crate::auth::store::CredentialStore;
use crate::error::{CliError, CliResult};
use crate::render::OutputMode;

use super::Session;

/// The unauthenticated probe timeout. Generous enough to ride an Aurora scale-from-zero resume on
/// `/readyz` without a false negative.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Outcome of a single status-code probe against an open endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// 2xx.
    Ok,
    /// A non-2xx response (e.g. `/readyz` → 503 when a dependency is down).
    Bad(u16),
    /// No response at all, and which of DNS / TLS / timeout / connection refused it was.
    Unreachable(&'static str),
}

impl Probe {
    fn is_ok(self) -> bool {
        matches!(self, Probe::Ok)
    }

    /// A short label for human and JSON output (stable: scripts read it).
    fn label(self) -> String {
        match self {
            Probe::Ok => "ok".to_string(),
            Probe::Bad(code) => format!("http {code}"),
            Probe::Unreachable(_) => "unreachable".to_string(),
        }
    }

    /// The label, with why an unreachable endpoint was, for the error a failing probe ends in.
    fn detail(self) -> String {
        match self {
            Probe::Unreachable(why) => format!("unreachable: {why}"),
            other => other.label(),
        }
    }
}

/// GET `path` on the deployment and classify the result as a [`Probe`]. Never returns an error:
/// an unreachable endpoint is a health signal, not a command failure. With `verbose`, the probe
/// is logged as every API call is.
async fn probe(http: &reqwest::Client, base: &str, path: &str, verbose: bool) -> Probe {
    let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
    let result = match http.get(&url).timeout(PROBE_TIMEOUT).send().await {
        Ok(resp) if resp.status().is_success() => Probe::Ok,
        Ok(resp) => Probe::Bad(resp.status().as_u16()),
        Err(e) => Probe::Unreachable(crate::error::transport_detail(&e)),
    };
    if verbose {
        eprintln!("[biffix] GET {url} → {}", result.detail());
    }
    result
}

/// `biffix health`: probe `/healthz` + `/readyz`, plus `/status` when signed in.
pub async fn health(session: &mut Session) -> CliResult<()> {
    let healthz = probe(&session.http, &session.api_base, "/healthz", session.verbose).await;
    let readyz = probe(&session.http, &session.api_base, "/readyz", session.verbose).await;

    // The component report needs auth. Include it only when a credential is already cached and the
    // API accepts it; a missing login, an unreadable credential store, or a rejected token is not
    // a health failure — just omit it.
    let signed_in = matches!(session.store.load(&session.profile_name), Ok(Some(_)));
    let status = if signed_in {
        match session.authed_client().await {
            Ok(client) => client.get_status().await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };

    render(session.output, &session.api_base, healthz, readyz, status.as_ref());

    // Liveness AND readiness must both pass for a clean exit; otherwise surface an Unavailable
    // (exit 7) carrying the failing probe(s).
    if healthz.is_ok() && readyz.is_ok() {
        Ok(())
    } else {
        Err(CliError::Unavailable(format!(
            "deployment not healthy (healthz: {}, readyz: {})",
            healthz.detail(),
            readyz.detail()
        )))
    }
}

fn render(
    mode: OutputMode,
    base: &str,
    healthz: Probe,
    readyz: Probe,
    status: Option<&StatusReport>,
) {
    match mode {
        OutputMode::Json => {
            crate::render::print_json(&serde_json::json!({
                "api_base": base,
                "healthy": healthz.is_ok() && readyz.is_ok(),
                "healthz": healthz.label(),
                "readyz": readyz.label(),
                "status": status,
            }));
        }
        OutputMode::Human => {
            println!("{base}");
            println!("  healthz: {}", healthz.label());
            println!("  readyz:  {}", readyz.label());
            if let Some(report) = status {
                println!("  status:  {}", report.status);
                for component in &report.components {
                    match &component.detail {
                        Some(detail) => {
                            println!("    - {}: {} ({detail})", component.name, component.status);
                        }
                        None => println!("    - {}: {}", component.name, component.status),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ok_is_ok() {
        assert!(Probe::Ok.is_ok());
        assert!(!Probe::Bad(503).is_ok());
        assert!(!Probe::Unreachable("connection refused").is_ok());
    }

    /// A session against `base` whose credential store is `store_path` (never touching the
    /// user's real config or credentials).
    fn session(base: &str, store_path: std::path::PathBuf) -> Session {
        Session {
            config: crate::config::Config::default(),
            profile_name: "default".into(),
            api_base: base.into(),
            store: crate::auth::store::FileCredentialStore::new(store_path),
            http: reqwest::Client::new(),
            output: OutputMode::Json,
            verbose: false,
        }
    }

    async fn stub(readyz: axum::http::StatusCode) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/healthz", axum::routing::get(|| async { "ok" }))
            .route("/readyz", axum::routing::get(move || async move { readyz }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_failing_readiness_probe_is_unavailable_exit_7() {
        // The path CI and deploy.sh gate on.
        let dir = tempfile::tempdir().unwrap();
        let base = stub(axum::http::StatusCode::SERVICE_UNAVAILABLE).await;
        let err =
            health(&mut session(&base, dir.path().join("credentials.json"))).await.unwrap_err();
        assert!(
            matches!(&err, CliError::Unavailable(m) if m.contains("readyz: http 503")),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 7);
    }

    #[tokio::test]
    async fn an_unreachable_deployment_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let base = format!("http://127.0.0.1:{port}");
        let err =
            health(&mut session(&base, dir.path().join("credentials.json"))).await.unwrap_err();
        assert!(
            matches!(&err, CliError::Unavailable(m)
                if m.contains("healthz: unreachable: connection refused")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_corrupt_credential_store_is_not_a_health_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "not json").unwrap();
        let base = stub(axum::http::StatusCode::OK).await;
        health(&mut session(&base, path)).await.unwrap();
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(Probe::Ok.label(), "ok");
        assert_eq!(Probe::Bad(503).label(), "http 503");
        assert_eq!(Probe::Unreachable("timed out").label(), "unreachable");
    }
}
