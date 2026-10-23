//! Resolve an image tag to its digest in a public OCI registry (quay.io, ghcr.io, Docker Hub, ECR
//! Public, …), anonymously: a `HEAD` of the manifest (the registry reports the digest in
//! `Docker-Content-Digest`), following the registry's Bearer-token challenge when it asks for one.
//!
//! Registration pins every declared image to a digest before anything is imported, so the import
//! key is known up front and an image another version already imported is not imported again.
//!
//! The registry host comes from the caller, and the token endpoint (the challenge's `realm`) from
//! that registry, so both are untrusted URLs fetched from inside the deploy's network. A
//! [`RegistryClient`] therefore only talks HTTPS to public addresses: a URL whose host is
//! `localhost` or a loopback, private, link-local (incl. the `169.254.169.254` metadata endpoint),
//! unique-local or other non-public IP literal is refused before it is requested; a host name is
//! refused when DNS resolves it to such an address (checked by the client's resolver, so the
//! address checked is the one connected to); and a redirect is followed only to a URL that passes
//! the same checks, at most [`MAX_REDIRECTS`] times.

use std::error::Error as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use biffix_workflow::digest::ImageDigest;
use biffix_workflow::images::{ImageRef, DOCKER_HUB, MANIFEST_MEDIA_TYPES};
use reqwest::{StatusCode, Url};

/// The most redirects one request follows (each to a URL that passes the same checks).
pub(crate) const MAX_REDIRECTS: usize = 5;

/// How long one registry or token request may take.
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub(crate) enum OciError {
    /// The registry (or its token endpoint) answered with something unusable (no or a malformed
    /// digest, a bad token reply): a gateway fault (502).
    #[error("{image}: {message}")]
    Registry { image: String, message: String },
    /// The registry (or its token endpoint) is down, rate limiting or unreachable (a 429, a 5xx,
    /// a timeout): retryable (503).
    #[error("{image}: {message}")]
    Unavailable { image: String, message: String },
    #[error("{image}: not found in its registry (check the name and tag)")]
    NotFound { image: String },
    #[error("{image}: the registry requires credentials (private images are not supported yet)")]
    Unauthorized { image: String },
    /// The registry refused the reference itself (a 4xx other than 401/403/404/429).
    #[error("{image}: the registry rejected the reference (HTTP {status})")]
    Rejected { image: String, status: StatusCode },
    /// A URL biffix will not fetch (not HTTPS, or a non-public host).
    #[error("{image}: {message}")]
    Refused { image: String, message: String },
}

/// Why a URL is not fetched; carried through reqwest's error chain from the redirect policy and
/// the resolver so it is reported as [`OciError::Refused`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Refused(String);

/// What a [`RegistryClient`] may talk to. Production is [`Rules::PUBLIC`]; the only other rules
/// are the tests' (plain HTTP to their own loopback server).
#[derive(Clone, Copy, Debug)]
struct Rules {
    scheme: &'static str,
    allow_loopback: bool,
}

impl Rules {
    const PUBLIC: Self = Self { scheme: "https", allow_loopback: false };
}

/// An HTTP client for public OCI registries, with the SSRF restrictions of this module built in:
/// [`resolve_digest`] takes only this type, so it cannot be handed an unrestricted client.
#[derive(Clone, Debug)]
pub(crate) struct RegistryClient {
    http: reqwest::Client,
    rules: Rules,
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryClient {
    /// HTTPS to public addresses only.
    pub(crate) fn new() -> Self {
        Self::with_rules(Rules::PUBLIC)
    }

    fn with_rules(rules: Rules) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent("biffix")
            .https_only(rules.scheme == "https")
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error(Refused(format!("more than {MAX_REDIRECTS} redirects")));
                }
                match check_url(attempt.url(), rules) {
                    Ok(()) => attempt.follow(),
                    Err(refused) => attempt.error(refused),
                }
            }))
            .dns_resolver(Arc::new(PublicResolver { allow_loopback: rules.allow_loopback }))
            // Never through a proxy (reqwest honours HTTP(S)_PROXY by default): the proxy would
            // resolve the host, and the resolver's public-address check would not apply.
            .no_proxy()
            .build()
            .expect("a reqwest client with a resolver and redirect policy always builds");
        Self { http, rules }
    }
}

/// Whether `ip` is a public unicast address (or, when `allow_loopback`, a loopback one).
fn allowed_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    (allow_loopback && ip.is_loopback()) || is_public(ip)
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // An IPv4 address in IPv6 clothing (`::ffff:a.b.c.d`, NAT64 `64:ff9b::a.b.c.d`) is
            // judged as the IPv4 address it reaches.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return is_public_v4(Ipv4Addr::new(
                    (s[6] >> 8) as u8,
                    s[6] as u8,
                    (s[7] >> 8) as u8,
                    s[7] as u8,
                ));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7 (incl. EC2's fd00:ec2::254)
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8)) // documentation
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    !(v4.is_loopback() // 127/8
        || v4.is_private() // 10/8, 172.16/12, 192.168/16
        || v4.is_link_local() // 169.254/16: instance metadata, the Lambda runtime API
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || a == 0 // "this network"
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT 100.64/10
        || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18/15
        || a >= 240) // reserved
}

/// Refuse a URL of the wrong scheme or whose host is `localhost` or a non-public IP literal. A
/// host name is checked again, after DNS, by [`PublicResolver`].
fn check_url(url: &Url, rules: Rules) -> Result<(), Refused> {
    if url.scheme() != rules.scheme {
        return Err(Refused(format!(
            "refusing to fetch `{url}`: only {} URLs are allowed",
            rules.scheme
        )));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Refused(format!("refusing to fetch `{url}`: it has no host")))?;
    let host = host.trim_end_matches('.');
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let allowed = match IpAddr::from_str(bare) {
        Ok(ip) => allowed_ip(ip, rules.allow_loopback),
        Err(_) => {
            rules.allow_loopback
                || !(host.eq_ignore_ascii_case("localhost")
                    || host.to_ascii_lowercase().ends_with(".localhost"))
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(Refused(format!("refusing to fetch `{url}`: its host is not a public address")))
    }
}

/// Resolves host names with the system resolver and refuses any that resolve to a non-public
/// address, so a public-looking name cannot point the client inside the network.
struct PublicResolver {
    allow_loopback: bool,
}

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_loopback = self.allow_loopback;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if let Some(bad) = addrs.iter().find(|a| !allowed_ip(a.ip(), allow_loopback)) {
                return Err(Box::new(Refused(format!(
                    "refusing to fetch from `{host}`: it resolves to a non-public address ({})",
                    bad.ip()
                ))) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The host to talk to for `registry` (Docker Hub's API is not at `docker.io`).
pub(crate) fn api_host(registry: &str) -> &str {
    if registry == DOCKER_HUB {
        "registry-1.docker.io"
    } else {
        registry
    }
}

/// A `WWW-Authenticate: Bearer realm="…",service="…",scope="…"` challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BearerChallenge {
    pub(crate) realm: String,
    pub(crate) service: Option<String>,
    pub(crate) scope: Option<String>,
}

/// Parse a Bearer challenge; `None` for any other scheme, or one without a realm. The scheme and
/// parameter names are case-insensitive, and a quoted value may escape a character with `\` (a
/// quoted-pair: RFC 7235's quoted-string). A header listing several challenges is not split:
/// registries send one.
///
/// Hand-rolled on purpose: `oci-client` builds its own HTTP client (it takes none), which would
/// bypass [`RegistryClient`]'s address checks; this one header is all of the registry protocol
/// biffix needs.
pub(crate) fn parse_challenge(header: &str) -> Option<BearerChallenge> {
    let (scheme, rest) = header.trim().split_once(|c: char| c.is_ascii_whitespace())?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    // key="value" pairs, comma-separated; values may themselves contain commas (a scope list).
    let mut remaining = rest.trim();
    while let Some((key, after)) = remaining.split_once('=') {
        let key = key.trim().trim_start_matches(',').trim();
        let after = after.trim_start();
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            // Up to the closing quote, each `\x` read as `x`.
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let end = loop {
                match chars.next()? {
                    (_, '\\') => value.push(chars.next()?.1),
                    (at, '"') => break at,
                    (_, c) => value.push(c),
                }
            };
            (value, &quoted[end + 1..])
        } else {
            match after.find(',') {
                Some(end) => (after[..end].to_string(), &after[end..]),
                None => (after.to_string(), ""),
            }
        };
        match key.to_ascii_lowercase().as_str() {
            "realm" => realm = Some(value),
            "service" => service = Some(value),
            "scope" => scope = Some(value),
            _ => {}
        }
        remaining = next.trim_start_matches(',').trim();
    }
    Some(BearerChallenge { realm: realm?, service, scope })
}

/// The error for a manifest `HEAD` that ended in `status`.
fn status_error(image: &str, status: StatusCode) -> OciError {
    let image = image.to_string();
    match status {
        StatusCode::NOT_FOUND => OciError::NotFound { image },
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => OciError::Unauthorized { image },
        // Rate limiting is the registry's (retryable) state, not a bad reference.
        StatusCode::TOO_MANY_REQUESTS => {
            OciError::Unavailable { image, message: format!("HTTP {status}") }
        }
        s if s.is_client_error() => OciError::Rejected { image, status: s },
        s => OciError::Unavailable { image, message: format!("HTTP {s}") },
    }
}

/// A transport error, as [`OciError::Refused`] when this module's checks stopped it.
fn transport_error(image: &str, e: reqwest::Error) -> OciError {
    let mut source = e.source();
    while let Some(err) = source {
        if let Some(refused) = err.downcast_ref::<Refused>() {
            return OciError::Refused { image: image.to_string(), message: refused.0.clone() };
        }
        source = err.source();
    }
    OciError::Unavailable { image: image.to_string(), message: e.to_string() }
}

/// The digest `image` names in its (public) registry: a canonical `sha256:<64 lowercase hex>`.
pub(crate) async fn resolve_digest(
    client: &RegistryClient,
    image: &ImageRef,
) -> Result<String, OciError> {
    if let Some(digest) = &image.digest {
        return Ok(digest.clone());
    }
    let http = &client.http;
    let rules = client.rules;
    let shown = image.to_string();
    let registry_error = |message: String| OciError::Registry { image: shown.clone(), message };
    let refused = |r: Refused| OciError::Refused { image: shown.clone(), message: r.0 };
    let reference = image.tag.as_deref().unwrap_or("latest");
    let url = Url::parse(&format!(
        "{}://{}/v2/{}/manifests/{reference}",
        rules.scheme,
        api_host(&image.registry),
        image.repository
    ))
    .map_err(|e| refused(Refused(format!("not a registry URL: {e}"))))?;
    check_url(&url, rules).map_err(refused)?;
    let accept = MANIFEST_MEDIA_TYPES.join(", ");
    let head = |token: Option<String>| {
        let mut req = http.head(url.clone()).header(reqwest::header::ACCEPT, &accept);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        req.send()
    };
    let mut res = head(None).await.map_err(|e| transport_error(&shown, e))?;
    if res.status() == StatusCode::UNAUTHORIZED {
        let challenge = res
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|h| h.to_str().ok())
            .and_then(parse_challenge)
            .ok_or_else(|| OciError::Unauthorized { image: shown.clone() })?;
        let realm = Url::parse(&challenge.realm).map_err(|e| {
            registry_error(format!("the token endpoint `{}` is not a URL: {e}", challenge.realm))
        })?;
        check_url(&realm, rules).map_err(refused)?;
        let mut query = Vec::new();
        if let Some(service) = &challenge.service {
            query.push(("service", service.clone()));
        }
        // Some registries omit the scope from a HEAD's challenge; ask for pull on this repository.
        query.push((
            "scope",
            challenge
                .scope
                .clone()
                .unwrap_or_else(|| format!("repository:{}:pull", image.repository)),
        ));
        let token_res =
            http.get(realm).query(&query).send().await.map_err(|e| transport_error(&shown, e))?;
        // Only a refusal is "requires credentials"; a failing token endpoint is the registry's
        // fault (retryable), not the caller's.
        match token_res.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(OciError::Unauthorized { image: shown });
            }
            s => {
                return Err(OciError::Unavailable {
                    image: shown,
                    message: format!("the token endpoint returned HTTP {s}"),
                })
            }
        }
        let token: serde_json::Value = token_res
            .json()
            .await
            .map_err(|e| registry_error(format!("the token endpoint's reply: {e}")))?;
        let token = token
            .get("token")
            .or_else(|| token.get("access_token"))
            .and_then(|t| t.as_str())
            .map(str::to_string);
        res = head(token).await.map_err(|e| transport_error(&shown, e))?;
    }
    let status = res.status();
    if !status.is_success() {
        return Err(status_error(&shown, status));
    }
    let digest = res
        .headers()
        .get("docker-content-digest")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| registry_error("the registry reported no digest".into()))?;
    digest.parse::<ImageDigest>().map(ImageDigest::into_string).map_err(|e| {
        registry_error(format!("the registry reported an invalid digest `{digest}`: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::extract::{Request, State};
    use axum::http::{header, HeaderValue, Response};

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn docker_hub_is_served_from_registry_1() {
        assert_eq!(api_host("docker.io"), "registry-1.docker.io");
        assert_eq!(api_host("quay.io"), "quay.io");
    }

    #[test]
    fn bearer_challenges_parse_with_quoted_commas() {
        let c = parse_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/ubuntu:pull,push""#,
        )
        .unwrap();
        assert_eq!(c.realm, "https://auth.docker.io/token");
        assert_eq!(c.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(c.scope.as_deref(), Some("repository:library/ubuntu:pull,push"));
        let bare =
            parse_challenge(r#"Bearer realm="https://quay.io/v2/auth",service="quay.io""#).unwrap();
        assert_eq!(bare.scope, None);
        assert_eq!(parse_challenge(r#"Basic realm="x""#), None);
        assert_eq!(parse_challenge("Bearer service=\"x\""), None);
        // A quoted-pair: an escaped quote is part of the value, not its end.
        let escaped = parse_challenge(
            r#"Bearer realm="https://a/t?x=y",service="re\"g\\x",scope="repository:a/b:pull,push""#,
        )
        .unwrap();
        assert_eq!(escaped.service.as_deref(), Some(r#"re"g\x"#));
        assert_eq!(escaped.scope.as_deref(), Some("repository:a/b:pull,push"));
        // An escape at the very end leaves the value unterminated.
        assert_eq!(parse_challenge(r#"Bearer realm="https://x\"#), None);
    }

    #[test]
    fn bearer_challenges_parse_unquoted_values_and_any_case() {
        let unquoted = parse_challenge("Bearer realm=https://x/token,service=reg").unwrap();
        assert_eq!(unquoted.realm, "https://x/token");
        assert_eq!(unquoted.service.as_deref(), Some("reg"));
        // RFC 7235: the scheme and parameter names are case-insensitive.
        let lower = parse_challenge(r#"bearer realm="https://x/token""#).unwrap();
        assert_eq!(lower.realm, "https://x/token");
        let upper = parse_challenge(r#"BEARER Realm="https://x/token",Service="s""#).unwrap();
        assert_eq!(
            (upper.realm.as_str(), upper.service.as_deref()),
            ("https://x/token", Some("s"))
        );
        // A scheme that merely starts with "bearer", no parameters, an unterminated quote.
        assert_eq!(parse_challenge(r#"Bearerx realm="https://x""#), None);
        assert_eq!(parse_challenge("Bearer"), None);
        assert_eq!(parse_challenge(r#"Bearer realm="https://x"#), None);
    }

    #[test]
    fn only_public_https_urls_are_fetched() {
        let ok = |u: &str| check_url(&Url::parse(u).unwrap(), Rules::PUBLIC).is_ok();
        assert!(ok("https://auth.docker.io/token"));
        assert!(ok("https://8.8.8.8/token"));
        for bad in [
            "http://auth.docker.io/token",
            "https://localhost/token",
            "https://LOCALHOST./token",
            "https://api.localhost/token",
            "https://127.0.0.1/token",
            "https://2130706433/token", // 127.0.0.1, spelled as a number
            "https://10.0.0.5:8443/token",
            "https://172.16.0.1/token",
            "https://192.168.1.1/token",
            "https://169.254.169.254/latest/meta-data/",
            "https://169.254.170.2/v2/credentials", // ECS/Lambda credentials endpoint
            "https://100.64.0.1/",
            "https://0.0.0.0/",
            "https://[::1]/token",
            "https://[fd00:ec2::254]/token",
            "https://[fe80::1]/token",
            "https://[::ffff:169.254.169.254]/token",
            "https://[64:ff9b::a9fe:a9fe]/token",
            "file:///etc/passwd",
        ] {
            assert!(!ok(bad), "{bad} should be refused");
        }
    }

    #[tokio::test]
    async fn names_that_resolve_to_non_public_addresses_are_refused() {
        use reqwest::dns::Resolve;
        let resolver = PublicResolver { allow_loopback: false };
        let Err(err) = resolver.resolve("localhost".parse().unwrap()).await else {
            panic!("localhost resolved")
        };
        assert!(err.to_string().contains("non-public address"), "{err}");
        let loopback = PublicResolver { allow_loopback: true };
        assert!(loopback.resolve("localhost".parse().unwrap()).await.is_ok());
    }

    #[tokio::test]
    async fn the_public_client_never_contacts_internal_registries() {
        let client = RegistryClient::new();
        for registry in
            ["localhost:5000", "127.0.0.1:5000", "10.0.0.5:8443", "169.254.169.254", "[::1]:5000"]
        {
            let image = ImageRef::parse(&format!("{registry}/x/y:1")).unwrap();
            let err = resolve_digest(&client, &image).await.unwrap_err();
            assert!(matches!(err, OciError::Refused { .. }), "{registry}: {err}");
        }
    }

    // ── resolve_digest against a local registry ───────────────────────────────────────────────

    /// What the fake registry does.
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// The manifest HEAD answers anonymously.
        Anonymous,
        /// A Bearer challenge (no scope); the token endpoint answers with `access_token`.
        Challenge,
        /// A challenge whose token endpoint fails with this status.
        TokenFails(u16),
        /// A challenge whose realm is the metadata endpoint.
        MetadataRealm,
        /// A challenge whose realm is HTTPS (not what the loopback test rules allow).
        HttpsRealm,
        /// The manifest HEAD fails with this status.
        Status(u16),
        /// A success with this `Docker-Content-Digest` (or none).
        Digest(Option<&'static str>),
        /// A redirect to an internal address.
        RedirectInternal,
    }

    #[derive(Clone)]
    struct Fake {
        mode: Mode,
        base: String,
    }

    async fn registry(State(fake): State<Fake>, req: Request) -> Response<Body> {
        let reply = |status: u16| Response::builder().status(status);
        let path = req.uri().path().to_string();
        let digest = |r: axum::http::response::Builder| {
            r.header("docker-content-digest", DIGEST).body(Body::empty()).unwrap()
        };
        if path == "/token" {
            let query = req.uri().query().unwrap_or_default().to_string();
            assert!(query.contains("service=reg"), "{query}");
            // No scope in the challenge: the client asks for pull on the repository.
            assert!(query.contains("scope=repository%3Ax%2Fy%3Apull"), "{query}");
            return match fake.mode {
                Mode::TokenFails(s) => reply(s).body(Body::empty()).unwrap(),
                _ => reply(200)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"access_token":"t0k"}"#))
                    .unwrap(),
            };
        }
        assert_eq!(path, "/v2/x/y/manifests/1");
        let authorized = req.headers().get(header::AUTHORIZATION)
            == Some(&HeaderValue::from_static("Bearer t0k"));
        let challenge = |realm: String| {
            reply(401)
                .header(
                    header::WWW_AUTHENTICATE,
                    format!(r#"Bearer realm="{realm}",service="reg""#),
                )
                .body(Body::empty())
                .unwrap()
        };
        match fake.mode {
            Mode::Anonymous => digest(reply(200)),
            Mode::Challenge | Mode::TokenFails(_) if !authorized => {
                challenge(format!("{}/token", fake.base))
            }
            Mode::Challenge => digest(reply(200)),
            Mode::TokenFails(_) => reply(500).body(Body::empty()).unwrap(),
            Mode::MetadataRealm => challenge("http://169.254.169.254/latest/api/token".into()),
            Mode::HttpsRealm => challenge("https://127.0.0.1:1/token".into()),
            Mode::Status(s) => reply(s).body(Body::empty()).unwrap(),
            Mode::Digest(d) => {
                let mut r = reply(200);
                if let Some(d) = d {
                    r = r.header("docker-content-digest", d);
                }
                r.body(Body::empty()).unwrap()
            }
            Mode::RedirectInternal => reply(307)
                .header(header::LOCATION, "http://10.0.0.5/v2/x/y/manifests/1")
                .body(Body::empty())
                .unwrap(),
        }
    }

    /// Resolve `127.0.0.1:<port>/x/y:1` against a fake registry in `mode`.
    async fn resolve_against(mode: Mode) -> Result<String, OciError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .fallback(registry)
            .with_state(Fake { mode, base: format!("http://{addr}") });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // The test-only seam: plain HTTP to loopback, which production rules refuse.
        let client = RegistryClient::with_rules(Rules { scheme: "http", allow_loopback: true });
        let image = ImageRef::parse(&format!("{addr}/x/y:1")).unwrap();
        resolve_digest(&client, &image).await
    }

    #[tokio::test]
    async fn resolves_a_digest_anonymously_or_through_a_bearer_challenge() {
        assert_eq!(resolve_against(Mode::Anonymous).await.unwrap(), DIGEST);
        assert_eq!(resolve_against(Mode::Challenge).await.unwrap(), DIGEST);
        // A reference that already names a digest makes no request at all.
        let pinned = ImageRef::parse(&format!("127.0.0.1:1/x/y@{DIGEST}")).unwrap();
        assert_eq!(resolve_digest(&RegistryClient::new(), &pinned).await.unwrap(), DIGEST);
    }

    #[tokio::test]
    async fn a_failing_token_endpoint_is_a_registry_fault_and_a_refusing_one_needs_credentials() {
        for status in [500, 503] {
            let err = resolve_against(Mode::TokenFails(status)).await.unwrap_err();
            assert!(
                matches!(&err, OciError::Unavailable { message, .. } if message.contains(&status.to_string())),
                "{status}: {err}"
            );
        }
        for status in [401, 403] {
            let err = resolve_against(Mode::TokenFails(status)).await.unwrap_err();
            assert!(matches!(err, OciError::Unauthorized { .. }), "{status}: {err}");
        }
    }

    #[tokio::test]
    async fn a_realm_or_redirect_to_a_refused_url_is_not_followed() {
        for mode in [Mode::MetadataRealm, Mode::HttpsRealm, Mode::RedirectInternal] {
            let err = resolve_against(mode).await.unwrap_err();
            assert!(matches!(err, OciError::Refused { .. }), "{err}");
        }
    }

    #[tokio::test]
    async fn manifest_statuses_map_to_caller_or_registry_errors() {
        let err = |s| async move { resolve_against(Mode::Status(s)).await.unwrap_err() };
        assert!(matches!(err(404).await, OciError::NotFound { .. }));
        assert!(matches!(err(403).await, OciError::Unauthorized { .. }));
        // A 401 with no Bearer challenge: nothing to exchange for a token.
        assert!(matches!(err(401).await, OciError::Unauthorized { .. }));
        assert!(matches!(err(400).await, OciError::Rejected { .. }));
        assert!(matches!(err(429).await, OciError::Unavailable { .. }));
        assert!(matches!(err(502).await, OciError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn only_a_canonical_sha256_digest_is_accepted() {
        const UPPERCASE: &str =
            "sha256:0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF";
        for bad in [None, Some("sha256:abc"), Some("md5:0123"), Some(UPPERCASE)] {
            let err = resolve_against(Mode::Digest(bad)).await.unwrap_err();
            assert!(matches!(err, OciError::Registry { .. }), "{bad:?}: {err}");
        }
    }
}
