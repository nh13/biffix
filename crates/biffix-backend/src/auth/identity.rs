//! Resolving a human label (email/name) for an authenticated principal.
//!
//! Cognito ACCESS tokens (what the API validates) don't carry `email`, so the run submitter would
//! otherwise be an opaque `sub`. At submit — a low-frequency operation — we resolve the Cognito
//! `username` to its email/name and snapshot it onto the immutable run anchor. Resolution is
//! ALWAYS best-effort: a failure returns `None` and must never block a run.

use async_trait::async_trait;

/// The user-pool id is the last path segment of the Cognito issuer URL
/// (`https://cognito-idp.<region>.amazonaws.com/<pool-id>`). A Cognito pool id always contains `_`.
///
/// Ungated (not behind `aws`) and defined once so both the OAuth client configuration (built
/// without the `aws` feature) and the Cognito identity resolver derive the pool id the same way.
pub(crate) fn pool_id_from_issuer(issuer: &str) -> Option<String> {
    issuer
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && s.contains('_'))
        .map(str::to_string)
}

/// Resolve a Cognito username to a human label (email, else name). `None` on any failure.
#[async_trait]
pub(crate) trait IdentityResolver: Send + Sync {
    async fn label_for(&self, username: &str) -> Option<String>;
}

/// Resolves nothing (local/test composition, or auth disabled).
pub(crate) struct NoopIdentityResolver;

#[async_trait]
impl IdentityResolver for NoopIdentityResolver {
    async fn label_for(&self, _username: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::pool_id_from_issuer;

    #[test]
    fn the_pool_id_is_the_issuer_s_last_segment() {
        let iss = "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_AbC";
        assert_eq!(pool_id_from_issuer(iss).as_deref(), Some("us-east-1_AbC"));
        assert_eq!(
            pool_id_from_issuer(&format!("{iss}/")).as_deref(),
            Some("us-east-1_AbC"),
            "a trailing slash is ignored"
        );
        // Not a Cognito pool id (no `_`), or nothing there at all.
        for bad in ["https://issuer.example.org/tenant", "https://issuer.example.org/", ""] {
            assert_eq!(pool_id_from_issuer(bad), None, "{bad}");
        }
    }
}
