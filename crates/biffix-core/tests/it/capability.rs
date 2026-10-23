//! Capability-token properties.
//!
//! The token is the engine's only credential and the ingest route is edge-reachable, so forgery,
//! cross-run reuse, and expiry are the properties that matter.

use biffix_core::capability::{check_secret, mint, verify, CapabilityError};
use biffix_core::ids::RunId;

const SECRET: &[u8] = b"a-test-signing-secret";
const NOW: u64 = 1_000;
const LATER: u64 = 2_000;

#[test]
fn a_minted_token_verifies_to_its_own_run() {
    let run = RunId::new("run-abc");
    let token = mint(SECRET, &run, LATER);
    assert_eq!(verify(SECRET, &token, NOW).unwrap(), run);
}

#[test]
fn a_token_cannot_be_forged_without_the_secret() {
    let token = mint(b"attacker-secret", &RunId::new("run-abc"), LATER);
    assert_eq!(verify(SECRET, &token, NOW), Err(CapabilityError::BadSignature));
}

#[test]
fn the_run_id_cannot_be_swapped_for_another_run() {
    // The whole point: a token for run A must not authorize appends to run B. Rewriting the run
    // field invalidates the signature, so cross-run injection is impossible.
    let token = mint(SECRET, &RunId::new("run-a"), LATER);
    let (payload, _sig) = token.rsplit_once('.').unwrap();
    let mut fields: Vec<&str> = payload.split('.').collect();
    let other = hex::encode(b"run-b");
    fields[1] = &other;
    let forged = format!("{}.{}", fields.join("."), token.rsplit_once('.').unwrap().1);
    assert_eq!(verify(SECRET, &forged, NOW), Err(CapabilityError::BadSignature));
}

#[test]
fn expiry_cannot_be_extended() {
    let token = mint(SECRET, &RunId::new("run-abc"), LATER);
    let (payload, sig) = token.rsplit_once('.').unwrap();
    let mut fields: Vec<&str> = payload.split('.').collect();
    fields[2] = "9999999999";
    assert_eq!(
        verify(SECRET, &format!("{}.{sig}", fields.join(".")), NOW),
        Err(CapabilityError::BadSignature)
    );
}

#[test]
fn an_expired_token_is_rejected() {
    let token = mint(SECRET, &RunId::new("run-abc"), NOW);
    assert_eq!(verify(SECRET, &token, NOW), Err(CapabilityError::Expired));
    assert_eq!(verify(SECRET, &token, NOW + 1), Err(CapabilityError::Expired));
}

#[test]
fn malformed_and_unversioned_tokens_are_rejected() {
    assert_eq!(verify(SECRET, "", NOW), Err(CapabilityError::Malformed));
    assert_eq!(verify(SECRET, "garbage", NOW), Err(CapabilityError::Malformed));
    assert_eq!(
        verify(SECRET, "v2.aa.2000.deadbeef", NOW),
        Err(CapabilityError::UnsupportedVersion)
    );
}

#[test]
fn run_ids_with_awkward_characters_round_trip() {
    // Hex-encoding the run id means a '.' in it cannot break the field split.
    for id in ["run.with.dots", "run-with-dash", "RUN_123", "run:colon"] {
        let run = RunId::new(id);
        let token = mint(SECRET, &run, LATER);
        assert_eq!(verify(SECRET, &token, NOW).unwrap(), run, "id={id}");
    }
}

/// Re-sign `payload` (the first three token fields) with `secret`, as a correct minter would.
fn signed(secret: &[u8], payload: &str) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
    mac.update(payload.as_bytes());
    format!("{payload}.{}", hex::encode(mac.finalize().into_bytes()))
}

#[test]
fn an_empty_secret_is_rejected_at_mint_and_verify() {
    // An empty HMAC key is zero-padded into a fixed, public block: a token under it is forgeable.
    assert_eq!(check_secret(b""), Err(CapabilityError::EmptySecret));
    assert_eq!(check_secret(SECRET), Ok(()));
    // Even a token genuinely signed under the empty key is refused.
    let forged = signed(b"", &format!("v1.{}.{LATER}", hex::encode(b"run-abc")));
    assert_eq!(verify(b"", &forged, NOW), Err(CapabilityError::EmptySecret));
}

#[test]
#[should_panic(expected = "must not be empty")]
fn minting_with_an_empty_secret_panics_rather_than_issue_a_forgeable_token() {
    let _ = mint(b"", &RunId::new("run-abc"), LATER);
}

#[test]
fn the_signature_is_checked_before_the_expiry() {
    // A bad signature on an expired or unparseable-expiry token is a BadSignature, not Expired /
    // Malformed: nothing downstream of the MAC acts on unverified input.
    let expired = mint(b"attacker-secret", &RunId::new("run-abc"), NOW);
    assert_eq!(verify(SECRET, &expired, NOW + 1), Err(CapabilityError::BadSignature));
    let bad_expiry =
        signed(b"attacker-secret", &format!("v1.{}.not-a-number", hex::encode(b"run-abc")));
    assert_eq!(verify(SECRET, &bad_expiry, NOW), Err(CapabilityError::BadSignature));
    // The same unparseable expiry, correctly signed, is Malformed — so the case above really was
    // decided by the signature.
    let signed_bad_expiry = signed(SECRET, &format!("v1.{}.not-a-number", hex::encode(b"run-abc")));
    assert_eq!(verify(SECRET, &signed_bad_expiry, NOW), Err(CapabilityError::Malformed));
}

#[test]
fn a_non_hex_signature_is_a_bad_signature() {
    let token = mint(SECRET, &RunId::new("run-abc"), LATER);
    let (payload, _sig) = token.rsplit_once('.').unwrap();
    assert_eq!(
        verify(SECRET, &format!("{payload}.zz-not-hex"), NOW),
        Err(CapabilityError::BadSignature)
    );
}

#[test]
fn a_correctly_signed_token_for_an_empty_run_id_is_malformed() {
    let token = mint(SECRET, &RunId::new(""), LATER);
    assert_eq!(verify(SECRET, &token, NOW), Err(CapabilityError::Malformed));
}

#[test]
fn a_correctly_signed_run_field_that_is_not_hex_utf8_is_malformed() {
    // Not hex at all, and hex whose bytes are not UTF-8: never an empty or lossy run id.
    for run_hex in ["zz", "ff"] {
        let token = signed(SECRET, &format!("v1.{run_hex}.{LATER}"));
        assert_eq!(verify(SECRET, &token, NOW), Err(CapabilityError::Malformed), "{run_hex}");
    }
}
