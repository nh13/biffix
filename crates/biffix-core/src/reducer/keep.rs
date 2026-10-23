//! Order-independent merges for the fold's single-valued facts: each keeps the value a replay in
//! log order would leave, whatever order the events are folded in.
use crate::event::LogSeq;

/// Set `slot` to `value` if unset, else keep the lexicographic min — an order-independent latch for
/// a fact that should only ever have one value.
pub(super) fn keep_min(slot: &mut Option<String>, value: Option<&str>) {
    if let Some(value) = value {
        if slot.as_deref().is_none_or(|current| value < current) {
            *slot = Some(value.to_string());
        }
    }
}

/// Set `slot` to `value` if unset, else to `min(slot, value)` — the order-independent latch for a
/// numeric fact that should only ever have one value.
pub(super) fn keep_min_by<T: Copy>(slot: &mut Option<T>, value: Option<T>, min: fn(T, T) -> T) {
    if let Some(value) = value {
        *slot = Some(slot.map_or(value, |current| min(current, value)));
    }
}

/// The `(ts micros, seq)` of the event a latest-wins field was taken from.
pub(super) type Stamp = (i64, Option<LogSeq>);

/// Set `slot` to `value` if its event is later than the one `slot` came from: the latest `(ts,
/// seq)` wins, and an exact tie keeps the lexicographic max. The result is the value a replay in
/// log order would leave with a last-write, but it no longer depends on the fold order.
pub(super) fn keep_latest<T: Clone>(
    slot: &mut Option<T>,
    at: &mut Option<Stamp>,
    value: &T,
    stamp: Stamp,
    key: fn(&T) -> &str,
) {
    let newer = match (at.as_ref(), slot.as_ref()) {
        (Some(current_at), Some(current)) => (stamp, key(value)) > (*current_at, key(current)),
        _ => true,
    };
    if newer {
        *slot = Some(value.clone());
        *at = Some(stamp);
    }
}

/// Set `slot` to the earliest of its value and `micros` — the order-independent "first time" latch.
pub(super) fn keep_min_micros(slot: &mut Option<i64>, micros: i64) {
    keep_min_by(slot, Some(micros), i64::min);
}
