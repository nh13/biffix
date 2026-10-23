//! PKCE (`S256`) and `state` generation — pure, so it is exhaustively unit/property-testable.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::secret::Secret;

/// A freshly generated PKCE pair plus the CSRF `state`. `verifier` and `state` are secrets that
/// must never be logged or persisted to disk (they live only for the duration of one login).
pub struct Pkce {
    pub(crate) verifier: Secret<String>,
    pub(crate) challenge: String,
    pub state: Secret<String>,
}

/// 32 random bytes → 43-char base64url (within RFC 7636's 43–128 range).
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    // The thread-local RNG, a CSPRNG seeded from the OS.
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

impl Pkce {
    /// Generate a `code_verifier`, its `S256` challenge, and a random `state`.
    pub fn generate() -> Self {
        let verifier = random_token();
        let challenge = challenge_for(&verifier);
        Pkce { verifier: Secret::new(verifier), challenge, state: Secret::new(random_token()) }
    }
}

/// The `S256` challenge for a given verifier: the one place the challenge is computed, called by
/// [`Pkce::generate`] and to cross-check it in tests.
pub(crate) fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Constant-time equality for `state`: comparison time does not depend on the mismatch
/// position, so a returned `state` cannot be probed byte-by-byte.
pub fn state_matches(expected: &str, actual: &str) -> bool {
    let a = expected.as_bytes();
    let b = actual.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_s256_of_verifier() {
        // An independently computed vector (`openssl dgst -sha256 -binary | base64url`, no
        // padding), so a wrong hash, the standard alphabet or padding would all fail — comparing
        // `challenge_for` with itself cannot.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mJ92IyROydlGkUrTQl6DxXu-Yd4-W8"),
            "jAW80-ZvLgsasF6rTCMkHt5gQuim8u51vc37DUWhJss"
        );
        let pkce = Pkce::generate();
        assert_eq!(pkce.challenge, challenge_for(pkce.verifier.expose()));
    }

    #[test]
    fn generated_tokens_are_unique_and_url_safe() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier.expose(), b.verifier.expose());
        assert_ne!(a.state.expose(), b.state.expose());
        // base64url, no padding: only [A-Za-z0-9_-].
        assert!(a
            .verifier
            .expose()
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn state_comparison_is_exact() {
        assert!(state_matches("abc123", "abc123"));
        assert!(!state_matches("abc123", "abc124"));
        assert!(!state_matches("abc", "abc123"));
        assert!(!state_matches("", "x"));
    }
}
