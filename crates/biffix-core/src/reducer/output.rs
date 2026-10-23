//! A run's output set: one [`OutputProjection`] per object, merged from `task_outputs` events, and
//! the stable API id of each.

use crate::event::{OutputRef, OutputRole};
use crate::ids::Attempt;
use crate::reducer::keep::keep_min;

/// One object in a run's output set, folded from `task_outputs` events. Every
/// field merges with a commutative, idempotent rule (min / max / AND), so the set is a pure
/// function of the observed events regardless of delivery order.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputProjection {
    pub s3_uri: String,
    /// The engine's display path: lexicographic min of the values observed.
    pub path: Option<String>,
    /// The task credited with the object: lexicographic min of the values observed (two tasks
    /// claiming one object is a workflow bug; min keeps it deterministic).
    pub task_key: Option<String>,
    /// The highest attempt that reported the object.
    pub attempt: Option<Attempt>,
    /// The highest-ranked role any event claimed ([`OutputRole::rank`]).
    pub role: OutputRole,
    /// True only when every event that reported the object said it was reused from cache: any
    /// "produced this run" claim wins.
    pub reused: bool,
    /// Max observed (engines rarely know these; the read side fills them from S3 instead).
    pub etag: Option<String>,
    pub size: Option<u64>,
}

impl OutputProjection {
    pub(super) fn new(uri: &str, role: OutputRole, reused: bool) -> Self {
        Self {
            s3_uri: uri.to_string(),
            path: None,
            task_key: None,
            attempt: None,
            role,
            reused,
            etag: None,
            size: None,
        }
    }

    pub(super) fn merge(
        &mut self,
        task_key: Option<&str>,
        attempt: Option<Attempt>,
        role: &OutputRole,
        reused: bool,
        r: &OutputRef,
    ) {
        keep_min(&mut self.path, r.path.as_deref());
        keep_min(&mut self.task_key, task_key);
        self.attempt = self.attempt.max(attempt);
        if role.rank() > self.role.rank() {
            self.role = role.clone();
        }
        self.reused &= reused;
        if r.etag.is_some() && r.etag > self.etag {
            self.etag = r.etag.clone();
        }
        self.size = self.size.max(r.size);
    }
}

/// The opaque, deterministic API id of a run's output object: the first 24 hex chars of
/// `sha256(run_id || "\n" || s3_uri)`. Stable across refolds, and it keeps raw keys out of URLs.
pub fn output_id(run_id: &str, s3_uri: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(run_id.as_bytes());
    h.update(b"\n");
    h.update(s3_uri.as_bytes());
    hex::encode(h.finalize())[..24].to_string()
}
