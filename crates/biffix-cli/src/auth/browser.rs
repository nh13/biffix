//! The browser login flow: bind a loopback listener on a registered port, open the browser, and
//! receive the `?code=…&state=…` redirect.
//!
//! The listener answers exactly `/callback`, returns a "you can close this tab" page, ignores
//! every other request without cancelling the pending login, and the whole wait is bounded
//! by a timeout. `state` is compared in constant time and a mismatch aborts without writing
//! anything.
//!
//! The registered redirect URIs name `localhost` (the Cognito CLI client's callbacks), which a
//! browser may resolve to `127.0.0.1` or `::1`. So a port is bound on both loopback addresses: a
//! port some other process holds on `::1` is treated as busy (else that process would receive the
//! authorization code), and a host without IPv6 falls back to `127.0.0.1` alone.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::error::{CliError, CliResult};

use super::paste::AuthCallback;

/// The default wait for the redirect before giving up.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long one accepted connection may take to send its request line. A connection that sends
/// nothing (a browser's speculative preconnect, a port probe) is dropped after this, so it cannot
/// hold the one-at-a-time accept loop while the real redirect waits in the backlog.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

const SUCCESS_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8>\
<title>biffix</title></head><body style=\"font-family:system-ui;padding:3rem\">\
<h2>You're signed in to biffix.</h2><p>You can close this tab and return to the terminal.</p>\
</body></html>";

/// A bound loopback listener (IPv4, plus IPv6 where the host has it) and the exact `redirect_uri`
/// that must be sent to the authorization server (Cognito matches it verbatim).
pub(crate) struct Loopback {
    v4: TcpListener,
    v6: Option<TcpListener>,
    pub(crate) redirect_uri: String,
}

/// Bind the first free port from the registered redirect-URI pool. Fails with the ports tried when
/// all are occupied — an unregistered port cannot be a redirect URI.
pub(crate) async fn bind_from_pool(redirect_uris: &[String]) -> CliResult<Loopback> {
    if redirect_uris.is_empty() {
        return Err(CliError::Unclassified(
            "no registered CLI redirect URIs discovered; the deployment may not have a CLI client"
                .into(),
        ));
    }
    let mut tried = Vec::new();
    for raw in redirect_uris {
        let url = Url::parse(raw).map_err(|e| {
            CliError::Unclassified(format!("invalid registered redirect URI {raw}: {e}"))
        })?;
        // `port_or_known_default` fills in the scheme's default (80/443) when the URI omits an
        // explicit port — `port()` alone returns None there and would wrongly bind 80 for https.
        let port = url.port_or_known_default().unwrap_or(80);
        tried.push(port);
        let Ok(v4) = TcpListener::bind(("127.0.0.1", port)).await else {
            continue;
        };
        let v6 = match TcpListener::bind(("::1", port)).await {
            Ok(l) => Some(l),
            // Someone else holds the port on ::1: a browser resolving `localhost` there would
            // hand them the code. Busy; try the next port.
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            // No IPv6 loopback on this host: `localhost` can only mean 127.0.0.1.
            Err(_) => None,
        };
        return Ok(Loopback { v4, v6, redirect_uri: raw.clone() });
    }
    Err(CliError::Unavailable(format!(
        "all registered loopback ports are busy (tried {tried:?}); close whatever is using them and retry"
    )))
}

/// Build the authorization URL for the browser (authorization-code + PKCE `S256`).
pub(crate) fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> CliResult<String> {
    let mut url = Url::parse(authorization_endpoint)
        .map_err(|e| CliError::Unclassified(format!("invalid authorization endpoint: {e}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", "openid email")
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

impl Loopback {
    /// Accept the next connection on either loopback address.
    async fn accept(&self) -> std::io::Result<TcpStream> {
        let (socket, _) = match &self.v6 {
            Some(v6) => tokio::select! {
                r = self.v4.accept() => r?,
                r = v6.accept() => r?,
            },
            None => self.v4.accept().await?,
        };
        Ok(socket)
    }

    /// Wait for the authorization redirect, verifying `state` in constant time. Non-`/callback`
    /// requests (and connections that send nothing) are answered or dropped without ending the
    /// wait; the whole wait is bounded.
    pub(crate) async fn wait_for_code(
        self,
        expected_state: &str,
        timeout: Duration,
    ) -> CliResult<AuthCallback> {
        let accept = async {
            loop {
                let mut socket = self
                    .accept()
                    .await
                    .map_err(|e| CliError::Unavailable(format!("loopback accept failed: {e}")))?;

                let target = match tokio::time::timeout(
                    READ_TIMEOUT,
                    read_request_target(&mut socket),
                )
                .await
                {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        respond(&mut socket, "400 Bad Request", "text/plain", "bad request").await;
                        continue;
                    }
                    // Silent connection: drop it and keep waiting for the real redirect.
                    Err(_) => continue,
                };

                // Only /callback is the redirect; anything else (favicon, probes) is ignored
                // without cancelling the pending login.
                let path = target.split('?').next().unwrap_or("");
                if path != "/callback" {
                    respond(&mut socket, "404 Not Found", "text/plain", "not the biffix callback")
                        .await;
                    continue;
                }

                let cb = match parse_callback_target(&target) {
                    Ok(cb) => cb,
                    Err(e) => {
                        respond(&mut socket, "400 Bad Request", "text/plain", "invalid callback")
                            .await;
                        return Err(e);
                    }
                };
                // The same constant-time check the paste flow uses; a mismatch aborts without
                // persisting anything.
                return match cb.verify(expected_state) {
                    Ok(()) => {
                        respond(&mut socket, "200 OK", "text/html; charset=utf-8", SUCCESS_PAGE)
                            .await;
                        Ok(cb)
                    }
                    Err(e) => {
                        respond(
                            &mut socket,
                            "400 Bad Request",
                            "text/plain",
                            "sign-in did not complete",
                        )
                        .await;
                        Err(e)
                    }
                };
            }
        };

        match tokio::time::timeout(timeout, accept).await {
            Ok(result) => result,
            Err(_) => Err(CliError::Unavailable(format!(
                "timed out after {}s waiting for the browser redirect; retry, or use --no-browser",
                timeout.as_secs()
            ))),
        }
    }
}

/// Read just enough of the HTTP request to extract the request target from the first line: until
/// that line's end (a request line may arrive over several reads), the connection's end, or a
/// full buffer.
async fn read_request_target(socket: &mut TcpStream) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    while !buf[..len].contains(&b'\n') && len < buf.len() {
        let n = socket.read(&mut buf[len..]).await.ok()?;
        if n == 0 {
            break;
        }
        len += n;
    }
    let head = String::from_utf8_lossy(&buf[..len]);
    let first_line = head.lines().next()?;
    // "GET /callback?code=…&state=… HTTP/1.1"
    let mut parts = first_line.split_whitespace();
    let _method = parts.next()?;
    parts.next().map(str::to_string)
}

/// Parse the `code`/`state`/`error` from a request target like `/callback?code=…&state=…`.
fn parse_callback_target(target: &str) -> CliResult<AuthCallback> {
    // Reuse the paste parser by forming an absolute URL from the loopback target.
    let absolute = format!("http://localhost{target}");
    super::paste::parse_redirect_url(&absolute)
}

/// Answer the browser with `status` and a `content_type` `body`, then close.
async fn respond(socket: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

/// Open the given URL in the user's default browser. Returns whether a browser could be launched.
pub(crate) fn open_browser(url: &str) -> bool {
    open::that(url).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_has_pkce_and_state() {
        let url = authorize_url(
            "https://auth.example/oauth2/authorize",
            "cli-1",
            "http://localhost:8976/callback",
            "state-abc",
            "challenge-xyz",
        )
        .unwrap();
        let url = Url::parse(&url).unwrap();
        assert_eq!(
            (url.scheme(), url.host_str(), url.path()),
            ("https", Some("auth.example"), "/oauth2/authorize")
        );
        // Every pair, each once: a duplicated or missing one is a different request.
        let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        pairs.sort();
        let mut expected: Vec<(String, String)> = [
            ("client_id", "cli-1"),
            ("code_challenge", "challenge-xyz"),
            ("code_challenge_method", "S256"),
            ("redirect_uri", "http://localhost:8976/callback"),
            ("response_type", "code"),
            ("scope", "openid email"),
            ("state", "state-abc"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        expected.sort();
        assert_eq!(pairs, expected);
    }

    fn uri(port: u16) -> String {
        format!("http://localhost:{port}/callback")
    }

    /// A port that is free right now (bound then released).
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn an_empty_pool_is_a_missing_cli_client() {
        let err = bind_from_pool(&[]).await.err().unwrap();
        assert!(matches!(err, CliError::Unclassified(_)), "{err:?}");
    }

    #[tokio::test]
    async fn an_occupied_port_falls_through_to_the_next_in_the_pool() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = held.local_addr().unwrap().port();
        let free = free_port();
        let lb = bind_from_pool(&[uri(busy), uri(free)]).await.unwrap();
        assert_eq!(lb.redirect_uri, uri(free));
    }

    #[tokio::test]
    async fn a_port_held_on_ipv6_loopback_counts_as_busy() {
        // Only meaningful where the host has an IPv6 loopback.
        let Ok(held) = std::net::TcpListener::bind("[::1]:0") else {
            return;
        };
        let busy = held.local_addr().unwrap().port();
        if std::net::TcpListener::bind(("127.0.0.1", busy)).is_err() {
            return; // the v4 side happens to be taken too; nothing to distinguish
        }
        let free = free_port();
        let lb = bind_from_pool(&[uri(busy), uri(free)]).await.unwrap();
        assert_eq!(lb.redirect_uri, uri(free));
    }

    #[tokio::test]
    async fn an_all_busy_pool_is_unavailable() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = held.local_addr().unwrap().port();
        let err = bind_from_pool(&[uri(busy)]).await.err().unwrap();
        assert!(matches!(err, CliError::Unavailable(_)), "{err:?}");
    }

    /// An ephemeral IPv4-only loopback and its port.
    async fn loopback() -> (Loopback, u16) {
        let v4 = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = v4.local_addr().unwrap().port();
        (Loopback { v4, v6: None, redirect_uri: uri(port) }, port)
    }

    /// Bind an ephemeral loopback and send it one raw HTTP request line, returning what
    /// `wait_for_code` yields for the given expected state.
    async fn drive_callback(
        expected_state: &'static str,
        request_line: &'static str,
        timeout: Duration,
    ) -> CliResult<AuthCallback> {
        let (lb, port) = loopback().await;
        let handle = tokio::spawn(async move { lb.wait_for_code(expected_state, timeout).await });
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(format!("{request_line}\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        handle.await.unwrap()
    }

    #[tokio::test]
    async fn callback_delivers_code_on_matching_state() {
        let cb = drive_callback(
            "good-state",
            "GET /callback?code=the-code&state=good-state HTTP/1.1",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(cb.code, "the-code");
        assert_eq!(cb.state.as_deref(), Some("good-state"));
    }

    #[tokio::test]
    async fn callback_rejects_mismatched_or_missing_state_without_a_code() {
        // The security-relevant branch: a differing or absent `state` aborts with
        // InvalidInput and no code is returned.
        for line in [
            "GET /callback?code=the-code&state=WRONG HTTP/1.1",
            "GET /callback?code=the-code HTTP/1.1",
        ] {
            let err = drive_callback("good-state", line, Duration::from_secs(5)).await.unwrap_err();
            assert!(matches!(err, CliError::InvalidInput(_)), "{line}: {err:?}");
        }
    }

    #[tokio::test]
    async fn a_request_line_split_across_reads_still_delivers_the_code() {
        let (lb, port) = loopback().await;
        let handle =
            tokio::spawn(
                async move { lb.wait_for_code("good-state", Duration::from_secs(5)).await },
            );
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client.write_all(b"GET /callback?code=c&sta").await.unwrap();
        client.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        client.write_all(b"te=good-state HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let cb = handle.await.unwrap().unwrap();
        assert_eq!(cb.state.as_deref(), Some("good-state"));
    }

    #[tokio::test]
    async fn an_idle_connection_does_not_block_the_real_callback() {
        let (lb, port) = loopback().await;
        let handle =
            tokio::spawn(
                async move { lb.wait_for_code("good-state", Duration::from_secs(30)).await },
            );
        // A preconnect that never sends a byte, held open for the whole test.
        let _idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"GET /callback?code=c&state=good-state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        // Delivered once the idle socket's read timeout expires — well inside the login timeout.
        let cb = tokio::time::timeout(READ_TIMEOUT * 3, handle)
            .await
            .expect("the idle connection blocked the callback")
            .unwrap()
            .unwrap();
        assert_eq!(cb.code, "c");
    }

    #[tokio::test]
    async fn non_callback_requests_are_ignored_then_the_callback_delivers() {
        // A favicon (or any non-/callback) probe is answered 404 and does NOT end the wait;
        // the subsequent real /callback still delivers the code on the same listener.
        let (lb, port) = loopback().await;
        let handle =
            tokio::spawn(
                async move { lb.wait_for_code("good-state", Duration::from_secs(5)).await },
            );

        // First hit: a favicon probe. It is answered (404) and the wait is still pending.
        let mut probe = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        probe.write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut resp = Vec::new();
        probe.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("404"), "favicon probe should get a 404");
        assert!(!handle.is_finished(), "the login wait must not end on a probe");

        // Second hit: the real callback delivers the code.
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"GET /callback?code=c&state=good-state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let cb = handle.await.unwrap().unwrap();
        assert_eq!(cb.code, "c");
    }

    #[tokio::test]
    async fn wait_for_code_times_out_when_no_redirect_arrives() {
        // With no client ever connecting, the bounded wait returns Unavailable, not a hang.
        let (lb, _) = loopback().await;
        let err = lb.wait_for_code("good-state", Duration::from_millis(50)).await.unwrap_err();
        assert!(matches!(err, CliError::Unavailable(_)), "got {err:?}");
    }
}
