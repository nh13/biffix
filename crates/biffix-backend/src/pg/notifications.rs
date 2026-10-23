//! [`NotificationStore`] for [`PgStore`]: `notifications`, `notification_reads` and
//! `notification_dismissals`.

use async_trait::async_trait;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use super::runs::WORKFLOW_TITLE_SQL;
use super::{micros_to_secs, PgStore};
use crate::store::notifications::NotificationStore;
use crate::store::StoreError;

/// The run `r` has a `failed` notification for the user bound as `user_param` (say `$4`) that they
/// have not dismissed (`notification_dismissals`; a read leaves it): it needs their attention
/// (`RunQuery::attention`). One predicate for the run list and the experiments' `needs_attention`.
pub(super) fn undismissed_failed_of_run(user_param: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM notifications n \
         WHERE n.run_id = r.run_id AND n.user_sub = {user_param} AND n.kind = 'failed' \
           AND NOT EXISTS (SELECT 1 FROM notification_dismissals nd \
                            WHERE nd.user_sub = n.user_sub AND nd.notification_id = n.id))"
    )
}

impl PgStore {
    /// How many of the user's notifications are unread, and how many of those are of failed runs.
    async fn unread_counts(&self, user: &str) -> Result<biffix_api::UnreadCounts, StoreError> {
        let row = sqlx::query(
            "SELECT count(*) AS unread, count(*) FILTER (WHERE n.kind = 'failed') AS unread_failed \
               FROM notifications n \
              WHERE n.user_sub = $1 \
                AND NOT EXISTS (SELECT 1 FROM notification_reads nr \
                                 WHERE nr.user_sub = n.user_sub AND nr.notification_id = n.id)",
        )
        .bind(user)
        .fetch_one(&self.pool)
        .await?;
        Ok(biffix_api::UnreadCounts {
            unread: row.get("unread"),
            unread_failed: row.get("unread_failed"),
        })
    }
}

#[async_trait]
impl NotificationStore for PgStore {
    async fn list_notifications(
        &self,
        user: &str,
        limit: i64,
    ) -> Result<biffix_api::NotificationList, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT n.id, n.run_id, n.kind, n.at, (nr.notification_id IS NOT NULL) AS read, \
                    r.run_name, w.name AS workflow_name, {WORKFLOW_TITLE_SQL} AS workflow_title, \
                    r.submitted_at, r.samples_total, r.samples_failed \
               FROM notifications n \
               LEFT JOIN notification_reads nr \
                      ON nr.user_sub = n.user_sub AND nr.notification_id = n.id \
               LEFT JOIN run_projection r ON r.run_id = n.run_id \
               LEFT JOIN workflow_versions w ON w.id = r.workflow_version \
              WHERE n.user_sub = $1 \
              ORDER BY n.at DESC, n.id COLLATE \"C\" DESC LIMIT $2"
        )))
        .bind(user)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .iter()
            .map(|r| biffix_api::Notification {
                id: r.get("id"),
                run_id: r.get::<String, _>("run_id").into(),
                run_name: r.get("run_name"),
                workflow_name: r.get("workflow_name"),
                workflow_title: r.get("workflow_title"),
                submitted_at: micros_to_secs(r.get("submitted_at")),
                kind: r.get("kind"),
                at: micros_to_secs(Some(r.get("at"))).unwrap_or_default(),
                read: r.get("read"),
                samples_total: r.get::<Option<i32>, _>("samples_total").map(i64::from),
                samples_failed: r.get::<Option<i32>, _>("samples_failed").map(i64::from),
            })
            .collect();
        let unread = self.unread_counts(user).await?;
        Ok(biffix_api::NotificationList {
            items,
            unread: unread.unread,
            unread_failed: unread.unread_failed,
        })
    }

    async fn mark_notifications_read(
        &self,
        user: &str,
        selection: &crate::store::notifications::NotificationSelection,
    ) -> Result<biffix_api::UnreadCounts, StoreError> {
        use crate::store::notifications::NotificationSelection;
        // Only the user's own notifications are marked: the selection is matched against theirs,
        // so an id or run that is not theirs marks nothing (and says nothing about it).
        let (ids, runs, all): (Option<&[String]>, Option<&[String]>, bool) = match selection {
            NotificationSelection::Ids(ids) => (Some(ids), None, false),
            NotificationSelection::Runs(runs) => (None, Some(runs), false),
            NotificationSelection::All => (None, None, true),
        };
        sqlx::query(
            "INSERT INTO notification_reads (user_sub, notification_id) \
             SELECT n.user_sub, n.id FROM notifications n \
              WHERE n.user_sub = $1 \
                AND ($4 OR n.id = ANY($2) OR n.run_id = ANY($3)) \
             ON CONFLICT DO NOTHING",
        )
        .bind(user)
        .bind(ids)
        .bind(runs)
        .bind(all)
        .execute(&self.pool)
        .await?;
        self.unread_counts(user).await
    }

    async fn read_notification_ahead(
        &self,
        user: &str,
        notification_id: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO notification_reads (user_sub, notification_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(user)
        .bind(notification_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn dismiss_attention(
        &self,
        user: &str,
        run_ids: &[String],
        via: crate::store::notifications::DismissVia,
    ) -> Result<(), StoreError> {
        // Only the user's own `failed` notifications are dismissed: the runs are matched against
        // theirs, so a run that is not theirs (or did not fail) dismisses nothing. The first
        // dismissal is kept, with its `via` and time.
        sqlx::query(
            "INSERT INTO notification_dismissals (user_sub, notification_id, via) \
             SELECT n.user_sub, n.id, $3 FROM notifications n \
              WHERE n.user_sub = $1 AND n.kind = 'failed' AND n.run_id = ANY($2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(user)
        .bind(run_ids)
        .bind(via.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
