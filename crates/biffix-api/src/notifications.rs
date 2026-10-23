//! `notifications` — the in-app bell (`/me/notifications`): a run's terminal transition, told to its
//! submitter, and whether they have read it.

use serde::{Deserialize, Serialize};

use biffix_core::ids::RunId;

/// One notification: a run of the caller's ended (or failed before it launched).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Notification {
    /// Stable id (one per run and kind; a replay rebuilds the same id).
    pub id: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub run_id: RunId,
    /// The run's name, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    /// The workflow's name (the Lab view calls it the pipeline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    /// The pipeline's friendly title (its form's `title`), when its author gave one; with
    /// `submitted_at` and the sample count it names a run that has no `run_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_title: Option<String>,
    /// When the run was submitted (epoch seconds), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<f64>,
    /// `succeeded`, `failed` or `cancelled` (open: a reader shows an unknown kind plainly).
    pub kind: String,
    /// When the run ended (epoch seconds).
    pub at: f64,
    /// The caller has read it (opened the run, marked it read, or tried the run again). Read state is
    /// the bell's alone: a failed run read still needs attention until it is dismissed.
    pub read: bool,
    /// The run's sample counts when it has per-sample progress, for "12 samples" / "1 sample
    /// failed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples_total: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples_failed: Option<i64>,
}

/// `GET /me/notifications` — the caller's latest notifications, newest first, and how many of all
/// of theirs are unread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationList {
    pub items: Vec<Notification>,
    pub unread: i64,
    /// How many of the caller's failed runs are unread (the bell's; *Needs attention* counts the
    /// failed runs not dismissed, as the run list's `attention` filter reads it).
    pub unread_failed: i64,
}

/// `GET /me/notifications` query: how many to return.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct NotificationsQuery {
    /// How many (default 20, clamped to 1..=100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// `POST /me/notifications/read` — mark some of the caller's notifications read. Give exactly one
/// of `ids`, `run_ids` (every notification of those runs) or `all: true`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationsReadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Vec<String>>))]
    pub run_ids: Option<Vec<RunId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<bool>,
}

/// The reply to a read: the caller's unread counts after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnreadCounts {
    pub unread: i64,
    pub unread_failed: i64,
}

/// `POST /me/attention/dismiss` — take some of the caller's failed runs out of their *Needs
/// attention* (one row's Dismiss, or *Dismiss all* naming exactly the runs it shows). Opening a run
/// or reading its notification does not; this does, as trying the run again does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AttentionDismissRequest {
    /// The runs to dismiss (1 to 500). A run that is not the caller's, or does not need their
    /// attention, is ignored.
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub run_ids: Vec<RunId>,
}

/// The reply to a dismissal: how many of the caller's runs still need their attention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AttentionCount {
    /// Absent when the dismissal was recorded but the count could not be read afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<i64>,
}
