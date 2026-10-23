//! The test-only static JWT verifier (`verify_claims`, the `test-support` feature) and bearer
//! parsing. A local HS256 key exercises the signature, algorithm, issuer, audience and expiry
//! checks without Cognito; the production Cognito verifier's own tests are in `src/auth/tests.rs`,
//! and the HTTP layer's use of a verifier is `http_auth.rs`.

use biffix_backend::auth::{bearer_token, verify_claims, AuthError};
use biffix_backend::testing::{self, now, signed_token};
use jsonwebtoken::{Algorithm, DecodingKey};
use serde_json::json;

const SECRET: &[u8] = b"test-secret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/pool";
const AUD: &str = "biffix-web";

fn token(claims: serde_json::Value) -> String {
    testing::hs256_token(SECRET, &claims)
}

fn key() -> DecodingKey {
    DecodingKey::from_secret(SECRET)
}

/// `t` verified as an HS256 token signed with [`SECRET`], issued by [`ISS`] for [`AUD`].
fn verify(t: &str) -> Result<biffix_backend::auth::Claims, AuthError> {
    verify_claims(t, &key(), ISS, AUD, Algorithm::HS256)
}

#[test]
fn valid_token_yields_claims_and_groups() {
    let t = token(json!({
        "sub": "user-1", "email": "bio@lab.example.org",
        "cognito:groups": ["biffix-operators"],
        "iss": ISS, "aud": AUD, "exp": now() + 3600
    }));
    let claims = verify(&t).unwrap();
    assert_eq!(claims.sub, "user-1");
    assert_eq!(claims.groups, vec!["biffix-operators"]);
}

#[test]
fn expired_token_is_rejected() {
    let t = token(json!({
        "sub": "u", "iss": ISS, "aud": AUD, "exp": now() - 3600
    }));
    assert_eq!(verify(&t), Err(AuthError::InvalidToken));
}

#[test]
fn wrong_audience_is_rejected() {
    let t = token(json!({
        "sub": "u", "iss": ISS, "aud": "some-other-app", "exp": now() + 3600
    }));
    assert_eq!(verify(&t), Err(AuthError::InvalidToken));
}

#[test]
fn wrong_issuer_is_rejected() {
    let t = token(json!({
        "sub": "u", "iss": "https://evil.example", "aud": AUD, "exp": now() + 3600
    }));
    assert_eq!(verify(&t), Err(AuthError::InvalidToken));
}

fn valid_claims() -> serde_json::Value {
    json!({"sub": "u", "iss": ISS, "aud": AUD, "exp": now() + 3600})
}

#[test]
fn a_well_formed_token_signed_with_another_key_is_rejected() {
    let forged = signed_token(&valid_claims(), Algorithm::HS256, b"other-secret");
    assert_eq!(verify(&forged), Err(AuthError::InvalidToken));
}

#[test]
fn a_token_signed_with_another_algorithm_is_rejected() {
    // The right secret, but HS512: the verifier accepts only the algorithm it was given.
    let t = signed_token(&valid_claims(), Algorithm::HS512, SECRET);
    assert_eq!(verify(&t), Err(AuthError::InvalidToken));
    // An unsigned token (`alg: none`, empty signature) carrying otherwise valid claims.
    let signed = token(valid_claims());
    let claims = signed.split('.').nth(1).unwrap();
    // base64url of {"alg":"none","typ":"JWT"}
    let unsigned = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{claims}.");
    assert_eq!(verify(&unsigned), Err(AuthError::InvalidToken));
}

#[test]
fn a_token_without_exp_is_rejected() {
    // A correctly signed credential that never expires.
    let t = token(json!({"sub": "u", "iss": ISS, "aud": AUD}));
    assert_eq!(verify(&t), Err(AuthError::InvalidToken));
}

#[test]
fn tampered_signature_is_rejected() {
    let mut t = token(json!({"sub": "u", "iss": ISS, "aud": AUD, "exp": now() + 3600}));
    t.push('x'); // corrupt
    assert_eq!(verify(&t), Err(AuthError::InvalidToken));
}

#[test]
fn a_token_without_iss_or_aud_is_rejected() {
    // jsonwebtoken skips an absent `iss`/`aud` unless they are required claims.
    let no_iss = token(json!({"sub": "u", "aud": AUD, "exp": now() + 3600}));
    assert_eq!(verify(&no_iss), Err(AuthError::InvalidToken));
    let no_aud = token(json!({"sub": "u", "iss": ISS, "exp": now() + 3600}));
    assert_eq!(verify(&no_aud), Err(AuthError::InvalidToken));
}

#[test]
fn bearer_parsing() {
    assert_eq!(bearer_token(Some("Bearer abc.def.ghi")).unwrap(), "abc.def.ghi");
    assert_eq!(bearer_token(None), Err(AuthError::MissingToken));
    assert_eq!(bearer_token(Some("Basic xyz")), Err(AuthError::MissingToken));
    // The scheme is case-insensitive (RFC 7235); surrounding whitespace is not part of the token.
    assert_eq!(bearer_token(Some("bearer abc")), Ok("abc"));
    assert_eq!(bearer_token(Some("BEARER  abc ")), Ok("abc"));
    // An empty or whitespace-only token is no token.
    for header in ["Bearer ", "Bearer    ", "Bearer", "", "Bearerabc"] {
        assert_eq!(bearer_token(Some(header)), Err(AuthError::MissingToken), "{header:?}");
    }
}
