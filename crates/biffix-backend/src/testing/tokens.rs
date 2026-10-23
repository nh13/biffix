//! Statically signed principals: HMAC-signed tokens and the [`AuthContext`] that verifies them.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::auth::rbac::{Rbac, Role};
use crate::AuthContext;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header};
use serde_json::Value;

/// Now, in whole epoch seconds (a JWT's `exp` unit).
pub fn now() -> u64 {
    biffix_core::clock::epoch_secs_u64()
}

/// `claims` signed with `secret` under `algorithm` (an HMAC one: `secret` is the raw key).
pub fn signed_token(claims: &Value, algorithm: Algorithm, secret: &[u8]) -> String {
    jsonwebtoken::encode(&Header::new(algorithm), claims, &EncodingKey::from_secret(secret))
        .unwrap()
}

/// `claims` signed HS256 with `secret`: what a [`static_auth`] context over `secret` verifies.
pub fn hs256_token(secret: &[u8], claims: &Value) -> String {
    signed_token(claims, Algorithm::HS256, secret)
}

/// A token for `sub` in `groups`, issued by `iss` for `aud` and expiring in an hour, signed HS256
/// with `secret`.
pub fn user_token(secret: &[u8], iss: &str, aud: &str, sub: &str, groups: &[&str]) -> String {
    hs256_token(
        secret,
        &serde_json::json!({
            "sub": sub, "cognito:groups": groups, "iss": iss, "aud": aud, "exp": now() + 3600
        }),
    )
}

/// An [`AuthContext`] that verifies HS256 tokens signed with `secret` (no Cognito), issued by
/// `issuer` for any of `audiences`, mapping each group in `groups` to its role.
pub fn static_auth(
    secret: &[u8],
    issuer: &str,
    audiences: Vec<String>,
    groups: &[(&str, Role)],
) -> AuthContext {
    AuthContext {
        verifier: crate::auth::JwtVerifier::static_key(
            DecodingKey::from_secret(secret),
            Algorithm::HS256,
        ),
        issuer: issuer.into(),
        audiences,
        rbac: Arc::new(Rbac::new(
            groups
                .iter()
                .map(|(group, role)| (group.to_string(), *role))
                .collect::<BTreeMap<_, _>>(),
        )),
    }
}
