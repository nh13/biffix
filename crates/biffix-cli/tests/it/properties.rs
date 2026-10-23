//! CLI correctness properties.

use std::time::{Duration, UNIX_EPOCH};

use biffix_cli::auth::pkce::{state_matches, Pkce};
use biffix_cli::auth::store::{Credentials, REFRESH_MARGIN};
use biffix_cli::params::assemble;
use biffix_cli::secret::Secret;
use proptest::prelude::*;

proptest! {
    /// 13.3 — param merge, as `runs submit` assembles them: inline always wins over the file, the
    /// file over what a resumed run inherited, a key from one source only keeps its value, and
    /// the result's keys are exactly the union of the inputs (no invented keys). The inline keys
    /// are some of the file's (so they collide) plus fresh ones; the inherited keys are drawn from
    /// names that may be either.
    #[test]
    fn merge_inline_wins_and_keys_are_the_union(
        inherited_keys in prop::collection::btree_set("[a-z]{1,8}", 0..6),
        file_keys in prop::collection::btree_set("[a-z]{1,6}", 0..6),
        overlap in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
        // Longer than any file key, so never one of them.
        fresh in prop::collection::btree_set("[a-z]{7,8}", 0..4),
    ) {
        let file_list: Vec<&String> = file_keys.iter().collect();
        let mut inline_keys: std::collections::BTreeSet<String> = fresh;
        if !file_list.is_empty() {
            inline_keys.extend(overlap.iter().map(|i| (*i.get(&file_list)).clone()));
        }

        let mut file = serde_json::Map::new();
        for k in &file_keys {
            file.insert(k.clone(), serde_json::json!("from_file"));
        }
        let inline_pairs: Vec<String> =
            inline_keys.iter().map(|k| format!("{k}=from_inline")).collect();
        let mut inherited = serde_json::Map::new();
        for k in &inherited_keys {
            inherited.insert(k.clone(), serde_json::json!("inherited"));
        }

        let obj = assemble(inherited, file, &inline_pairs, &[]).unwrap();

        // Keys are exactly the union.
        let mut expected: std::collections::BTreeSet<String> = file_keys.clone();
        expected.extend(inline_keys.iter().cloned());
        expected.extend(inherited_keys.iter().cloned());
        let actual: std::collections::BTreeSet<String> = obj.keys().cloned().collect();
        prop_assert_eq!(actual, expected);

        for (k, v) in &obj {
            let want = if inline_keys.contains(k) {
                "from_inline"
            } else if file_keys.contains(k) {
                "from_file"
            } else {
                "inherited"
            };
            prop_assert_eq!(v, &serde_json::json!(want), "{}", k);
        }
    }
}

proptest! {
    /// 13.5 — redaction: no rendered form of a `Secret` contains its value, for any value.
    #[test]
    fn secret_never_leaks_its_value(value in ".*") {
        let secret = Secret::new(value.clone());
        prop_assert_eq!(format!("{secret}"), "[redacted]");
        prop_assert_eq!(format!("{secret:?}"), "[redacted]");
        // The value is only reachable via the explicit escape hatch.
        prop_assert_eq!(secret.expose(), &value);
    }
}

fn expiring_at(expires_at: u64) -> Credentials {
    Credentials {
        access_token: Secret::new("a".into()),
        refresh_token: None,
        expires_at_unix: expires_at,
        subject: "s".into(),
        email: None,
        api_base_url: "https://x.example/api".into(),
    }
}

fn needs_refresh_at(expires_at: u64, now: u64) -> bool {
    expiring_at(expires_at).needs_refresh(UNIX_EPOCH + Duration::from_secs(now), REFRESH_MARGIN)
}

proptest! {
    /// 13.7 — the refresh margin: a token is refreshed from `REFRESH_MARGIN` before it expires on
    /// (`now + margin >= expires_at`), drawn around that boundary.
    #[test]
    fn refresh_is_due_from_the_margin_before_expiry(
        expires_at in REFRESH_MARGIN.as_secs() + 5..1_000_000,
        delta in -5i64..=5,
    ) {
        let now = (expires_at - REFRESH_MARGIN.as_secs()).checked_add_signed(delta).unwrap();
        prop_assert_eq!(needs_refresh_at(expires_at, now), delta >= 0);
    }

    /// 13.7 — monotonicity: once a refresh is due, it stays due at every later time.
    #[test]
    fn refresh_margin_is_monotonic(
        expires_at in 0u64..1_000_000,
        now in 0u64..1_000_000,
        later in 0u64..1_000_000,
    ) {
        if needs_refresh_at(expires_at, now) {
            prop_assert!(needs_refresh_at(expires_at, now + later));
        }
    }
}

#[test]
fn refresh_margin_boundary() {
    let margin = REFRESH_MARGIN.as_secs();
    let expires_at = 10_000;
    assert!(!needs_refresh_at(expires_at, expires_at - margin - 1));
    assert!(needs_refresh_at(expires_at, expires_at - margin));
    assert!(needs_refresh_at(expires_at, expires_at - margin + 1));
    // Already expired.
    assert!(needs_refresh_at(expires_at, expires_at + 1));
}

/// `state` with the byte at `i` replaced by another printable ASCII byte (`offset` in 1..=94 steps
/// through the other 94).
fn with_byte_changed(state: &str, i: usize, offset: u8) -> String {
    let mut bytes = state.as_bytes().to_vec();
    let orig = bytes[i];
    assert!((0x20..0x7f).contains(&orig), "state is printable ASCII");
    bytes[i] = 0x20 + (orig - 0x20 + offset) % 95;
    String::from_utf8(bytes).unwrap()
}

proptest! {
    /// 13.8 — state rejection: a `state` differing from the generated one is rejected (and the
    /// generated one is always accepted). The near misses — one byte changed at any position,
    /// one byte short, one byte long — reach the byte comparison, not just the length check.
    #[test]
    fn any_differing_state_is_rejected(
        idx in any::<prop::sample::Index>(),
        offset in 1u8..=94,
        extra in any::<char>(),
        other in ".*",
    ) {
        let pkce = Pkce::generate();
        let expected = pkce.state.expose();
        prop_assert!(state_matches(expected, expected));

        let changed = with_byte_changed(expected, idx.index(expected.len()), offset);
        prop_assert_eq!(changed.len(), expected.len());
        prop_assert!(!state_matches(expected, &changed), "{}", changed);
        prop_assert!(!state_matches(expected, &expected[..expected.len() - 1]));
        let longer = format!("{expected}{extra}");
        prop_assert!(!state_matches(expected, &longer));
        if &other != expected {
            prop_assert!(!state_matches(expected, &other));
        }
    }
}

#[test]
fn a_state_differing_only_in_its_first_or_last_byte_is_rejected() {
    let pkce = Pkce::generate();
    let expected = pkce.state.expose();
    for i in [0, expected.len() - 1] {
        let changed = with_byte_changed(expected, i, 1);
        assert!(!state_matches(expected, &changed), "byte {i}: {changed}");
    }
}
