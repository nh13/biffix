# biffix-core

The biffix RunEvent contract and its folds: the versioned event type every producer appends, the per-source wire formats, and the reducer that projects run, task and attempt state from the event log.

It is published so that the [`biffix`](https://crates.io/crates/biffix) CLI can be installed from
crates.io. It is an internal crate of [biffix](https://github.com/nh13/biffix), released in lockstep
with the rest of it, and its API may change in any release.
