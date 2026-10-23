//! A reconcile tick's work budgets: a fair share of a bounded amount of work, and the invocation's
//! deadline. Pure; the tick's steps check them before each unit of work (a listing page, a delete
//! batch, a pricing page) so a backlog drains over ticks instead of running the Lambda out of time.

use std::time::Duration;

/// How long a step must still have before it starts another unit of work: room for one more S3
/// page (list and delete) or pricing page, and for the steps after it to start.
pub const UNIT_RESERVE: Duration = Duration::from_secs(10);

/// How many of `remaining` units the next of `left` consumers (including it) may use: an even
/// share, rounded up. A consumer at the head of a list therefore cannot take the whole budget every
/// tick while the ones behind it wait; what one leaves unused carries over to the ones after it.
pub fn fair_share(remaining: usize, left: usize) -> usize {
    remaining.div_ceil(left.max(1))
}

/// When the invocation must be done by (since the epoch), from the Lambda context; `None` is
/// unbounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deadline(Option<Duration>);

impl Deadline {
    /// The deadline at `epoch_millis` (the Lambda context's `deadline`).
    pub fn at_epoch_millis(epoch_millis: u64) -> Self {
        Deadline(Some(Duration::from_millis(epoch_millis)))
    }

    /// No deadline.
    pub fn unbounded() -> Self {
        Deadline(None)
    }

    /// Whether a unit of work may start at `now` (since the epoch): at least [`UNIT_RESERVE`] is
    /// left before the deadline.
    pub fn allows_another(self, now: Duration) -> bool {
        self.0.is_none_or(|at| at.saturating_sub(now) >= UNIT_RESERVE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fair_share_keeps_a_huge_head_from_starving_the_rest() {
        assert_eq!(fair_share(8, 1), 8);
        assert_eq!(fair_share(8, 3), 3);
        assert_eq!(fair_share(8, 20), 1);
        assert_eq!(fair_share(0, 5), 0);
        assert_eq!(fair_share(5, 0), 5);
        // A tick: a huge head consumer, then small ones needing one unit each.
        let needs = [100, 1, 1, 1];
        let mut remaining = 8;
        let mut done = Vec::new();
        for (i, need) in needs.iter().enumerate() {
            let used = fair_share(remaining, needs.len() - i).min(*need);
            remaining -= used;
            done.push(used == *need);
        }
        assert_eq!(done, [false, true, true, true]);
    }

    #[test]
    fn work_starts_only_with_the_reserve_left() {
        let deadline = Deadline::at_epoch_millis(60_000);
        assert!(deadline.allows_another(Duration::from_secs(10)));
        assert!(deadline.allows_another(Duration::from_secs(50)));
        assert!(!deadline.allows_another(Duration::from_millis(50_001)));
        assert!(!deadline.allows_another(Duration::from_secs(70)));
        assert!(Deadline::unbounded().allows_another(Duration::from_secs(u64::MAX / 2)));
    }
}
