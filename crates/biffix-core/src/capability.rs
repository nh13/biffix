//! The per-run **capability token** the coordinator uses to POST RunEvents.
//!
//! The token is the only credential the engine plugin holds, and the ingest route is reachable from
//! the public edge, so it must be unforgeable: a guessable token would let anyone who knows or
//! guesses a run id inject events into that run's log, i.e. forge provenance in an append-only
//! store. It is HMAC-SHA256 signed over the run id and an expiry, with the secret held only by the
//! control plane.
//!
//! Minting and verification live together on purpose: a verifier that disagrees with the minter is
//! indistinguishable from a forgery, and that bug would surface only in production.
//!
//! Deliberately not a JWT: one issuer, one audience, two claims. A signed opaque token avoids an
//! algorithm-negotiation surface (`alg: none`) for no loss of function.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::ids::RunId;

type HmacSha256 = Hmac<Sha256>;

/// Wire-format version, so the signing scheme can change without ambiguity.
const VERSION: &str = "v1";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    #[error("capability token is malformed")]
    Malformed,
    #[error("capability token version is not supported")]
    UnsupportedVersion,
    #[error("capability token signature is invalid")]
    BadSignature,
    #[error("capability token has expired")]
    Expired,
    /// The signing secret is empty. Not a property of any token: the deployment is misconfigured.
    #[error("capability signing secret is empty")]
    EmptySecret,
}

/// Check that `secret` is usable as the signing key: non-empty.
///
/// HMAC accepts a key of any length, and an empty key is zero-padded into a fixed, publicly-known
/// block — so a secret misconfigured as `""` would make every token forgeable by anyone who knows
/// this (open-source) scheme. Callers should run this once at startup to fail closed there; `mint`
/// and `verify` enforce it again regardless.
///
/// Only emptiness is rejected here: a length floor measures length, not entropy, and the deployed
/// secret is a generated 64-character string. A stricter floor belongs at the startup check.
pub fn check_secret(secret: &[u8]) -> Result<(), CapabilityError> {
    if secret.is_empty() {
        return Err(CapabilityError::EmptySecret);
    }
    Ok(())
}

/// A keyed MAC over `secret`, which the caller has already passed through [`check_secret`].
fn keyed_mac(secret: &[u8]) -> HmacSha256 {
    HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length")
}

/// Mint a token authorizing appends for exactly `run_id`, valid until `expires_at_secs`.
///
/// # Panics
///
/// If `secret` is empty (see [`check_secret`]): minting a forgeable token must never succeed.
/// Validate the secret at startup with [`check_secret`].
pub fn mint(secret: &[u8], run_id: &RunId, expires_at_secs: u64) -> String {
    check_secret(secret).expect("capability signing secret must not be empty (see check_secret)");
    // The run id is hex-encoded so no character in it can confuse the field split.
    let payload =
        format!("{VERSION}.{}.{expires_at_secs}", hex::encode(run_id.as_str().as_bytes()));
    let mut mac = keyed_mac(secret);
    mac.update(payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    format!("{payload}.{signature}")
}

/// Verify a token and return the run id it authorizes.
///
/// `now_secs` is a parameter rather than a clock read so expiry is testable and the caller owns the
/// time source. An empty `secret` is refused with [`CapabilityError::EmptySecret`] before any token
/// is looked at.
pub fn verify(secret: &[u8], token: &str, now_secs: u64) -> Result<RunId, CapabilityError> {
    // An empty key is publicly known, so no token "verified" under it proves anything.
    check_secret(secret)?;
    let parts: Vec<&str> = token.split('.').collect();
    let [version, run_hex, expiry, signature] = parts.as_slice() else {
        return Err(CapabilityError::Malformed);
    };
    if *version != VERSION {
        return Err(CapabilityError::UnsupportedVersion);
    }

    // Signature first: nothing downstream may act on unverified input. Verify with the `hmac`
    // crate's own `Mac::verify_slice`, which does the constant-time compare (via `subtle`)
    // internally — no hand-rolled equality over the tag. A signature that is not valid hex is
    // simply a bad signature.
    let payload = format!("{version}.{run_hex}.{expiry}");
    let provided = hex::decode(signature).map_err(|_| CapabilityError::BadSignature)?;
    let mut mac = keyed_mac(secret);
    mac.update(payload.as_bytes());
    mac.verify_slice(&provided).map_err(|_| CapabilityError::BadSignature)?;

    let expires_at: u64 = expiry.parse().map_err(|_| CapabilityError::Malformed)?;
    if now_secs >= expires_at {
        return Err(CapabilityError::Expired);
    }
    let decoded = hex::decode(run_hex).map_err(|_| CapabilityError::Malformed)?;
    let run = String::from_utf8(decoded).map_err(|_| CapabilityError::Malformed)?;
    if run.is_empty() {
        return Err(CapabilityError::Malformed);
    }
    Ok(RunId::new(run))
}
