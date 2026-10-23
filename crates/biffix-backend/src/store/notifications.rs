//! [`NotificationStore`]: a user's notifications, the reads and the dismissals.

use async_trait::async_trait;

use super::StoreError;

/// Which of a user's notifications to mark read ([`NotificationStore::mark_notifications_read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationSelection {
    /// These notification ids (any that are not the user's are ignored).
    Ids(Vec<String>),
    /// Every notification of these runs.
    Runs(Vec<String>),
    /// All of them.
    All,
}

/// The most notifications one `GET /me/notifications` returns (the upper bound of its `limit`).
pub(crate) const MAX_NOTIFICATIONS: i64 = 100;

/// How a failed run left a user's *Needs attention* ([`NotificationStore::dismiss_attention`]),
/// recorded beside the dismissal so it can be told later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DismissVia {
    /// The user dismissed it (one row, or *Dismiss all*).
    Dismiss,
    /// The user tried the run again (a re-run or a resume of it was accepted).
    TryAgain,
}

impl DismissVia {
    /// The stored name (`notification_dismissals.via`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DismissVia::Dismiss => "dismiss",
            DismissVia::TryAgain => "try_again",
        }
    }
}

/// A user's notifications (the fold's `notifications`), and the user state kept beside them:
/// which they have read, and which failed runs they have taken out of *Needs attention*.
#[async_trait]
pub trait NotificationStore: Send + Sync {
    /// The user's newest notifications (by when the run ended, then id, descending), at most
    /// `limit` (taken as given: `GET /me/notifications` bounds it to `1..=`[`MAX_NOTIFICATIONS`]),
    /// each with whether the user has read it and
    /// its run's name, workflow name and sample counts; plus how many of ALL the user's
    /// notifications are unread, and how many of those are of failed runs.
    async fn list_notifications(
        &self,
        user: &str,
        limit: i64,
    ) -> Result<biffix_api::NotificationList, StoreError>;

    /// Mark the user's selected notifications read (idempotent; a selection naming notifications
    /// that are not the user's marks nothing for them) and return the user's unread counts after.
    async fn mark_notifications_read(
        &self,
        user: &str,
        selection: &NotificationSelection,
    ) -> Result<biffix_api::UnreadCounts, StoreError>;

    /// Record `notification_id` read for `user` whether or not the notification exists yet: a read
    /// written ahead of the fold, for a notification the caller knows the deterministic id of
    /// ([`biffix_core::reducer::notification_id`]) but the run has not produced (a user cancelling
    /// their own run is not told of it). Reads are user state, never touched by the fold, so a
    /// read of a notification the run never produces (it ends another way) is inert. Idempotent.
    async fn read_notification_ahead(
        &self,
        user: &str,
        notification_id: &str,
    ) -> Result<(), StoreError>;

    /// Take the user's failed runs `run_ids` out of their *Needs attention*: record a dismissal of
    /// each run's `failed` notification for them, with `via`. Idempotent (the first dismissal is
    /// the one kept); a run that is not the user's, or has no `failed` notification, dismisses
    /// nothing. Dismissals are user state, as reads are, never touched by the fold; reading a
    /// notification does not dismiss it, nor dismissing read it.
    async fn dismiss_attention(
        &self,
        user: &str,
        run_ids: &[String],
        via: DismissVia,
    ) -> Result<(), StoreError>;
}
