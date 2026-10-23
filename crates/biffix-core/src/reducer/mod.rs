//! Reducers (folds) — the read side.
//!
//! The invariant: `projection == fold(events_in_ts_order, initial)`. Incremental
//! and replay folds MUST agree. Every reducer step is idempotent and order-tolerant: a
//! duplicate or a state-regressing event is stored but does not move the projection backward.
//!
//! Two levels: an **Attempt** (keyed `batch_job_id`) advances monotonically, fed by
//! the two-stream join (engine owns semantics, AWS owns terminal ground truth). A **Task**
//! (keyed `run_id + task_key`) reduces over its attempts. A **Run** reduces over
//! control-plane + coordinator + engine events.

mod attempt;
mod fold;
mod keep;
mod output;
mod run;
mod task;

pub use attempt::AttemptProjection;
pub use fold::{is_trusted_producer, Projection};
pub use output::{output_id, OutputProjection};
pub use run::{notification_id, RunNotification, RunProjection, TaskOutcomeCounts};
pub use task::{TaskProjection, Verdict};

/// `map[key]`, inserting `new()` first when it is missing — without allocating the owned key on
/// the (common) hit path, as `entry` would.
fn get_or_insert_with<'m, V>(
    map: &'m mut std::collections::BTreeMap<String, V>,
    key: &str,
    new: impl FnOnce() -> V,
) -> &'m mut V {
    if !map.contains_key(key) {
        map.insert(key.to_string(), new());
    }
    map.get_mut(key).expect("inserted above")
}
