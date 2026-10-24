//! The single `CliError` → (message, exit code) mapping.
//!
//! Every failure the CLI surfaces names what failed and the next action, and maps to exactly one
//! documented exit code so CI can gate on the result. No bare stack trace is ever the primary
//! output.

/// Exit codes. `0` and `2` (clap usage) are produced outside this
/// enum; every other code has exactly one `CliError` family.
pub mod exit {
    pub const SUCCESS: i32 = 0;
    pub const UNCLASSIFIED: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const AUTH_REQUIRED: i32 = 3;
    pub const FORBIDDEN: i32 = 4;
    pub const NOT_FOUND: i32 = 5;
    pub const INVALID_INPUT: i32 = 6;
    pub const UNAVAILABLE: i32 = 7;
    pub const RUN_FAILED: i32 = 8;
    pub const RUN_CANCELLED: i32 = 9;
}

/// A classified CLI failure. The variant determines the exit code (`exit_code`) and carries a
/// human message that already names the next action.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// No usable credential; the user must `biffix login`. (401, missing/expired credential.)
    #[error("{0}")]
    AuthRequired(String),
    /// The caller's role is too low for the operation. (403.)
    #[error("{0}")]
    Forbidden(String),
    /// The referenced version/run/workflow does not exist. (404.)
    #[error("{0}")]
    NotFound(String),
    /// Local validation failed, or the server rejected the input. (local validation / 400.)
    #[error("{0}")]
    InvalidInput(String),
    /// The service is unreachable or unavailable. (503, DNS, TLS, timeout, connection refused.)
    #[error("{0}")]
    Unavailable(String),
    /// A followed run reached a terminal `failed` state.
    #[error("{0}")]
    RunFailed(String),
    /// A followed run reached a terminal `cancelled` state.
    #[error("{0}")]
    RunCancelled(String),
    /// Anything not otherwise classified.
    #[error("{0}")]
    Unclassified(String),
}

impl CliError {
    /// The documented exit code for this failure (exit-code totality: exactly one per variant).
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::AuthRequired(_) => exit::AUTH_REQUIRED,
            CliError::Forbidden(_) => exit::FORBIDDEN,
            CliError::NotFound(_) => exit::NOT_FOUND,
            CliError::InvalidInput(_) => exit::INVALID_INPUT,
            CliError::Unavailable(_) => exit::UNAVAILABLE,
            CliError::RunFailed(_) => exit::RUN_FAILED,
            CliError::RunCancelled(_) => exit::RUN_CANCELLED,
            CliError::Unclassified(_) => exit::UNCLASSIFIED,
        }
    }

    /// Classify a transport failure into `Unavailable`, naming the `target` that could not be
    /// reached and which of DNS / TLS / timeout / connection-refused it was, so both the
    /// OAuth token endpoint and the control-plane API report the host they were talking to.
    pub(crate) fn unreachable(target: &str, e: &reqwest::Error) -> Self {
        CliError::Unavailable(format!("cannot reach {target}: {}", transport_detail(e)))
    }

    /// This error with its message rewritten by `f`, keeping its kind (and so its exit code).
    pub(crate) fn map_message(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            CliError::AuthRequired(m) => CliError::AuthRequired(f(m)),
            CliError::Forbidden(m) => CliError::Forbidden(f(m)),
            CliError::NotFound(m) => CliError::NotFound(f(m)),
            CliError::InvalidInput(m) => CliError::InvalidInput(f(m)),
            CliError::Unavailable(m) => CliError::Unavailable(f(m)),
            CliError::RunFailed(m) => CliError::RunFailed(f(m)),
            CliError::RunCancelled(m) => CliError::RunCancelled(f(m)),
            CliError::Unclassified(m) => CliError::Unclassified(f(m)),
        }
    }

    /// Whether a failed poll may be a blip worth polling through (within [`TRANSIENT_BUDGET`]):
    /// the service unavailable (a 502/503/504, a timeout), or an unclassified failure such as a
    /// 500 from a Lambda crash. A refusal — 401, 403, 404, 400 — is final, and returned at once.
    pub(crate) fn is_transient(&self) -> bool {
        matches!(self, CliError::Unavailable(_) | CliError::Unclassified(_))
    }

    /// A short kind label for structured (JSON) diagnostics.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            CliError::AuthRequired(_) => "auth_required",
            CliError::Forbidden(_) => "forbidden",
            CliError::NotFound(_) => "not_found",
            CliError::InvalidInput(_) => "invalid_input",
            CliError::Unavailable(_) => "unavailable",
            CliError::RunFailed(_) => "run_failed",
            CliError::RunCancelled(_) => "run_cancelled",
            CliError::Unclassified(_) => "error",
        }
    }
}

pub type CliResult<T> = Result<T, CliError>;

/// Consecutive transient failures tolerated while polling (`runs follow`, `register --wait`)
/// before giving up: a blip does not become a spurious "run failed".
pub(crate) const TRANSIENT_BUDGET: u32 = 20;

/// A loopback address nothing listens on, for tests of an unreachable endpoint: a connection to it
/// is refused at once. Port 1 lies below every OS's ephemeral range, so no `bind(":0")` in this or
/// another process is ever handed it, unlike a port bound and released a moment ago, which a
/// concurrent stub can take (and, closing with the connection queued, turn the refusal into a
/// reset).
#[cfg(test)]
pub(crate) const CLOSED_LOOPBACK: &str = "127.0.0.1:1";

/// The kind of transport failure behind a reqwest error. reqwest itself flags only `is_timeout` /
/// `is_connect`, so the connect case is split by walking the source chain: an `io::Error` of kind
/// `ConnectionRefused`, hyper-util's resolver error (rendered `dns error: …`), or a rustls
/// certificate/handshake error.
pub(crate) fn transport_detail(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        return "timed out";
    }
    let (mut refused, mut dns, mut tls) = (false, false, false);
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => refused = true,
                std::io::ErrorKind::TimedOut => return "timed out",
                _ => {}
            }
        }
        let text = err.to_string().to_ascii_lowercase();
        dns |= text.contains("dns error") || text.contains("failed to lookup address");
        tls |= text.contains("certificate") || text.contains("tls") || text.contains("handshake");
        source = err.source();
    }
    if dns {
        "DNS lookup failed"
    } else if refused {
        "connection refused"
    } else if tls {
        "TLS handshake failed"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_request() {
        "the request failed in flight"
    } else {
        "transport error"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_maps_to_its_documented_code() {
        // Exit-code totality, pinned to the documented literal numbers: a
        // test comparing each variant with the same constant `exit_code()` returns could not catch
        // a changed or duplicated code.
        let all = [
            (CliError::AuthRequired("x".into()), 3, "auth_required"),
            (CliError::Forbidden("x".into()), 4, "forbidden"),
            (CliError::NotFound("x".into()), 5, "not_found"),
            (CliError::InvalidInput("x".into()), 6, "invalid_input"),
            (CliError::Unavailable("x".into()), 7, "unavailable"),
            (CliError::RunFailed("x".into()), 8, "run_failed"),
            (CliError::RunCancelled("x".into()), 9, "run_cancelled"),
            (CliError::Unclassified("x".into()), 1, "error"),
        ];
        for (err, code, kind) in &all {
            assert_eq!(err.exit_code(), *code, "{}", err.kind());
            assert_eq!(err.kind(), *kind);
        }
        let codes: std::collections::BTreeSet<i32> =
            all.iter().map(|(e, ..)| e.exit_code()).collect();
        let kinds: std::collections::BTreeSet<&str> = all.iter().map(|(e, ..)| e.kind()).collect();
        assert_eq!(codes.len(), all.len(), "exit codes must be distinct");
        assert_eq!(kinds.len(), all.len(), "JSON error kinds must be distinct");
        assert!(!codes.contains(&exit::USAGE) && !codes.contains(&exit::SUCCESS));
        assert_eq!((exit::SUCCESS, exit::USAGE), (0, 2));
    }

    #[test]
    fn only_a_blip_is_polled_through() {
        assert!(CliError::Unavailable("503".into()).is_transient());
        assert!(CliError::Unclassified("500".into()).is_transient());
        for refusal in [
            CliError::AuthRequired(String::new()),
            CliError::Forbidden(String::new()),
            CliError::NotFound(String::new()),
            CliError::InvalidInput(String::new()),
        ] {
            assert!(!refusal.is_transient(), "{refusal:?}");
        }
    }

    #[tokio::test]
    async fn a_refused_connection_is_named_as_such() {
        let err = reqwest::Client::new()
            .get(format!("http://{CLOSED_LOOPBACK}/"))
            .send()
            .await
            .unwrap_err();
        let CliError::Unavailable(m) = CliError::unreachable("the biffix API", &err) else {
            panic!("expected Unavailable");
        };
        assert!(m.contains("connection refused"), "got {m:?}");
        assert!(m.contains("the biffix API"), "got {m:?}");
    }
}
