//! Refusals the `biffix` binary gives before it talks to a deployment, with the exit code each
//! one documents.

use crate::common::biffix;

#[test]
fn a_blank_version_id_is_invalid_input_before_any_request() {
    for verb in ["show", "release", "unrelease"] {
        let home = tempfile::tempdir().unwrap();
        // No login: a command that went on to the API would stop at the credential (exit 3).
        let out = biffix(home.path())
            .args(["--url", "https://biffix.example.org/api", "workflows", verb, " "])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(6), "{verb}: {stderr}");
        assert!(stderr.contains("version id must not be blank"), "{verb}: {stderr}");
    }
}
