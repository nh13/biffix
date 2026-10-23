//! Correctness properties and golden vectors for the hashing and validation contracts the CLI and
//! the backend share: the workflow content hash and version id, the tenant image-cache keys, and
//! the param validator's verdict. They live in `biffix-workflow` so they hold for every caller; the
//! example-based validation cases are in `validate.rs` and the shared `xtask/src/cases/validate.rs`
//! table.

use biffix_workflow::formschema::{FormField, WidgetHint};
use biffix_workflow::validate::validate_params;
use biffix_workflow::version::{content_hash, source_id};
use proptest::prelude::*;

/// Arbitrary source file set: (path, bytes) pairs with distinct paths.
fn file_set() -> impl Strategy<Value = Vec<(String, Vec<u8>)>> {
    prop::collection::vec(
        ("[a-z][a-z0-9_/]{0,12}", prop::collection::vec(any::<u8>(), 0..32)),
        0..8,
    )
    .prop_map(|mut v| {
        // Dedup by path so a "reorder" is a true permutation, not a content change.
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup_by(|a, b| a.0 == b.0);
        v
    })
}

proptest! {
    /// 13.1 — content_hash is invariant under input ordering and stable across runs.
    #[test]
    fn content_hash_is_order_independent(
        (files, shuffled) in file_set().prop_flat_map(|f| (Just(f.clone()), Just(f).prop_shuffle()))
    ) {
        prop_assert_eq!(content_hash(files.clone()), content_hash(shuffled));
        // Deterministic across calls.
        prop_assert_eq!(content_hash(files.clone()), content_hash(files));
    }

    /// 13.1 — any byte change alters the hash, and so does renaming a file.
    #[test]
    fn content_hash_is_sensitive_to_byte_and_path_changes(
        files in file_set().prop_filter("non-empty", |f| !f.is_empty())
    ) {
        let before = content_hash(files.clone());
        let mut mutated = files.clone();
        mutated[0].1.push(0xFF); // append a byte to the first file
        prop_assert_ne!(&before, &content_hash(mutated));
        // A name no generated path can have (paths are lowercase), so the rename is a new path.
        let mut renamed = files;
        renamed[0].0.push('X');
        prop_assert_ne!(before, content_hash(renamed));
    }

    /// 13.2 — version identity: identical source + SHA ⇒ identical source_id, and it is exactly
    /// `{sha}:{content_hash}`.
    #[test]
    fn source_id_is_deterministic(sha in "[0-9a-f]{40}", files in file_set()) {
        let ch = content_hash(files);
        let id_a = source_id(&sha, &ch);
        let id_b = source_id(&sha, &ch);
        prop_assert_eq!(&id_a, &id_b);
        prop_assert_eq!(id_a, format!("{sha}:{ch}"));
    }
}

/// 13.2 — a fixed golden vector, shared with the server (same function). Pins the exact bytes so an
/// accidental change to the hashing discipline is caught, not just relative determinism.
#[test]
fn content_hash_golden_vector() {
    let files = vec![
        ("Snakefile".to_string(), b"rule all:\n".to_vec()),
        ("config/config.yaml".to_string(), b"reads: x\n".to_vec()),
    ];
    let ch = content_hash(files);
    // The pinned golden (sha256 over `path NUL sha256(bytes)` per sorted file, computed outside
    // this code). Changing it changes every version id: that must be deliberate.
    assert_eq!(ch, "4154d24779fab2f5395548db5e779c5e1f59123001beb644b0bf154699911a45");
    assert_eq!(
        source_id("0123456789abcdef0123456789abcdef01234567", &ch),
        "0123456789abcdef0123456789abcdef01234567:\
         4154d24779fab2f5395548db5e779c5e1f59123001beb644b0bf154699911a45"
    );
}

/// The tenant-wide image cache keys, pinned to values computed outside this code: a change to
/// their framing makes every cached import and derive look new, so it must be deliberate.
#[test]
fn image_keys_golden_vectors() {
    use biffix_workflow::images::{content_key, derived_key};
    let digest = format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        content_key(&digest),
        "687d55564dd7290f02edbbdd0ccb576861ca96a278e5b135d8bdc8ed60778804"
    );
    assert_eq!(
        derived_key(&digest, "rt1"),
        "a00337c9ed28311654b47eea7366a0386463d864fbde0d05edd05921af3d2540"
    );
    // A Dockerfile build's key, of the context tarball's sha256 `t` (the archs are sorted first,
    // so their order does not matter). Changing it rebuilds every tenant's built images, so it
    // changes only on purpose: the `build2:` scheme (the tarball's sha256, not the client's tree
    // hash) was one such change.
    use biffix_workflow::images::{build_content_key, Arch, BuildKind};
    assert_eq!(
        build_content_key(
            BuildKind::Dockerfile,
            "t",
            Some("Dockerfile"),
            &[Arch::Arm64, Arch::Amd64]
        ),
        "30d5609cc09f755f36a54b1f63fbd8e24a59d1cab6497111c3a1a35f973b8c7e"
    );
}

proptest! {
    /// 13.4 — validation agreement: the CLI's pre-flight check and the server call this one
    /// function, so their verdicts agree by construction; what is tested is that the verdict is
    /// the rule's. A required field that is absent or blank errors, an optional absent one does
    /// not, and any other present value errors iff it is not in the enum. The accepted value is drawn about half the
    /// time, so a validator that rejects every value fails.
    #[test]
    fn validation_verdict_follows_required_and_enum(
        required in any::<bool>(),
        present in any::<bool>(),
        value in prop_oneof![Just("ok".to_string()), "[a-z]{0,8}"],
    ) {
        let specs = vec![FormField {
            name: "p".into(),
            widget: WidgetHint::Text,
            required,
            description: None,
            enum_values: Some(vec!["ok".into()]),
            is_output: false,
            is_secret: false,
            is_path_input: false,
            ..Default::default()
        }];
        let params = if present {
            serde_json::json!({ "p": value })
        } else {
            serde_json::json!({})
        };
        let got: Vec<(String, String)> = validate_params(&specs, &params)
            .into_iter()
            .map(|e| (e.field, e.message))
            .collect();
        // A blank value is no value for a required field.
        let missing = !present || (required && value.trim().is_empty());
        let want: Vec<(String, String)> = match (missing, required, value == "ok") {
            (true, true, _) => vec![("p".into(), "required".into())],
            (true, false, _) | (false, _, true) => vec![],
            (false, _, false) => vec![("p".into(), "not one of ok".into())],
        };
        prop_assert_eq!(got, want);
    }
}
