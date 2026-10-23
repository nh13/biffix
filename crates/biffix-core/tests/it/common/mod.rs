//! Helpers shared by the reducer test modules.

use biffix_core::event::RunEvent;
use biffix_core::reducer::Projection;

/// Every ordering of `items` (Heap's algorithm). Guarded to small logs: the count is `n!`.
pub(crate) fn permutations(items: &[RunEvent]) -> Vec<Vec<RunEvent>> {
    fn go(k: usize, a: &mut Vec<RunEvent>, out: &mut Vec<Vec<RunEvent>>) {
        if k <= 1 {
            out.push(a.clone());
            return;
        }
        for i in 0..k {
            go(k - 1, a, out);
            if k.is_multiple_of(2) {
                a.swap(i, k - 1);
            } else {
                a.swap(0, k - 1);
            }
        }
    }
    assert!(items.len() <= 7, "permutation test kept small");
    let mut a = items.to_vec();
    let mut out = vec![];
    go(a.len(), &mut a, &mut out);
    out
}

/// Fold `events` incrementally in exactly the given order. Unlike [`Projection::replay`], which
/// sorts by `(ts, seq)` first, this exercises the reducer's own order-independence.
pub(crate) fn fold_in_order(events: &[RunEvent]) -> Projection {
    let mut p = Projection::new();
    for e in events {
        p.apply(e);
    }
    p
}
