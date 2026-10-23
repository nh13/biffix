//! Notifications, needs-attention and dismissals.

use super::*;

use biffix_backend::store::experiments::ExperimentReads;
use biffix_backend::store::notifications::NotificationStore;
use biffix_backend::store::runs::RunReads;
use biffix_core::testing::RunEventExt;

/// Through the single writer: alice's runs `ok` (succeeded at 10 s), `bad1` and `bad2` (failed at
/// 20 s and 30 s; `bad1` named, in workflow `rnaseq`) and `stop` (cancelled on her request at
/// 40 s), and bob's `theirs` (failed at 50 s).
async fn notified_pool(label: &str) -> PgStore {
    PgStore::new(notified_pg(label).await)
}

/// [`notified_pool`]'s database, for a test that appends more events to it.
async fn notified_pg(label: &str) -> sqlx::PgPool {
    use biffix_core::time::Timestamp;
    let pool = migrated_pool(label).await;
    exec_all(
        &pool,
        &["INSERT INTO workflow_versions (registration_inputs, registration_inputs_hash, \
             runtime_key, runtime_recipe, \
             id, name, project, git_sha, source_content_hash, \
             image_digest, engine, engine_version, entrypoint, source, form_schema, queue_classes, \
             created_at) \
             VALUES ('{}'::jsonb, '', '', '{}'::jsonb, \
             'wf1', 'rnaseq', 'p', 'abc', 'h', 'sha256:x', 'snakemake', '9', 'Snakefile', \
             '{}'::jsonb, '{}'::jsonb, '{}'::text[], 0)"],
    )
    .await;
    let at = Timestamp::from_epoch_seconds;
    let mut events = vec![];
    for (run, user, end, kind) in [
        ("ok", "alice", 10.0, "succeeded"),
        ("bad1", "alice", 20.0, "failed"),
        ("bad2", "alice", 30.0, "failed"),
        ("stop", "alice", 40.0, "cancelled"),
        ("theirs", "bob", 50.0, "failed"),
    ] {
        let rid = RunId::new(run);
        let mut anchor = biffix_core::event::RunSubmitted::new(
            "wf1",
            "sha",
            "img",
            serde_json::json!({}),
            vec![],
        );
        anchor.submitter = Some(format!("user:{user}"));
        anchor.submitter_label = Some(format!("{user}@lab.example.org"));
        if run == "bad1" {
            anchor.run_name = Some("batch-1".into());
        }
        events.push(biffix_core::event::run_submitted(&rid, at(end - 5.0), anchor));
        match kind {
            "succeeded" => events.push(
                biffix_core::testing::event(
                    end,
                    biffix_core::source::EventSource::Snakemake,
                    format!("dk-{run}-ok"),
                    biffix_core::event::EventData::RunSucceeded {},
                )
                .with_run(run),
            ),
            "failed" => events.push(biffix_core::event::run_failed(
                &rid,
                at(end),
                biffix_core::event::FAILURE_STAGE_LAUNCH,
                "no capacity",
            )),
            _ => {
                events.push(biffix_core::event::cancel_requested(&rid, at(end - 1.0), user, None));
                events.push(biffix_core::event::run_cancelled(&rid, at(end)));
            }
        }
    }
    biffix_db::log::append_and_advance(&pool, &events).await.unwrap();
    pool
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn lists_a_users_notifications_newest_first_with_their_runs_and_counts() {
    let store = notified_pool("lists_notifications").await;
    let list = store.list_notifications("alice", 20).await.unwrap();
    let ids: Vec<&str> = list.items.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["stop:cancelled", "bad2:failed", "bad1:failed", "ok:succeeded"]);
    let bad1 = &list.items[2];
    assert_eq!(
        (
            bad1.run_name.as_deref(),
            bad1.workflow_name.as_deref(),
            bad1.kind.as_str(),
            bad1.at,
            bad1.read
        ),
        (Some("batch-1"), Some("rnaseq"), "failed", 20.0, false)
    );
    assert_eq!((list.unread, list.unread_failed), (4, 2));
    // The limit bounds the items (the SQL `LIMIT`; the API clamps it), not the counts.
    let two = store.list_notifications("alice", 2).await.unwrap();
    let ids: Vec<&str> = two.items.iter().map(|n| n.id.as_str()).collect();
    assert_eq!((ids, two.unread), (vec!["stop:cancelled", "bad2:failed"], 4));
    assert!(store.list_notifications("nobody", 20).await.unwrap().items.is_empty());
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn marks_only_the_users_own_notifications_read() {
    use biffix_backend::store::notifications::NotificationSelection;
    let store = notified_pool("marks_notifications_read").await;
    // Bob's id names nothing of alice's.
    let after = store
        .mark_notifications_read("alice", &NotificationSelection::Ids(vec!["theirs:failed".into()]))
        .await
        .unwrap();
    assert_eq!((after.unread, after.unread_failed), (4, 2));
    let after = store
        .mark_notifications_read("alice", &NotificationSelection::Ids(vec!["ok:succeeded".into()]))
        .await
        .unwrap();
    assert_eq!((after.unread, after.unread_failed), (3, 2));
    let after = store
        .mark_notifications_read(
            "alice",
            &NotificationSelection::Runs(vec!["bad1".into(), "theirs".into()]),
        )
        .await
        .unwrap();
    assert_eq!((after.unread, after.unread_failed), (2, 1));
    // Idempotent.
    let again = store
        .mark_notifications_read("alice", &NotificationSelection::Runs(vec!["bad1".into()]))
        .await
        .unwrap();
    assert_eq!(again, after);
    let list = store.list_notifications("alice", 20).await.unwrap();
    let read: Vec<bool> = list.items.iter().map(|n| n.read).collect();
    assert_eq!(read, [false, false, true, true]);
    let all = store.mark_notifications_read("alice", &NotificationSelection::All).await.unwrap();
    assert_eq!((all.unread, all.unread_failed), (0, 0));
    // Bob's are untouched.
    let bob = store.list_notifications("bob", 20).await.unwrap();
    assert_eq!((bob.unread, bob.unread_failed), (1, 1));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn attention_lists_a_users_undismissed_failed_runs_with_the_true_total_and_pages() {
    use biffix_backend::store::notifications::{DismissVia, NotificationSelection};
    let store = notified_pool("attention_filter").await;
    let attention = RunQuery {
        submitter: Some("user:alice".into()),
        attention: Some("alice".into()),
        limit: 1,
        ..Default::default()
    };
    let page = store.list_runs(&attention).await.unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.runs[0].run_id.as_str(), "bad2");
    // Paging forward and back keeps the filter (the keyset parameters follow `attention`).
    let next = store
        .list_runs(&RunQuery { after: page.last_cursor.clone(), ..attention.clone() })
        .await
        .unwrap();
    assert_eq!((next.total, next.runs[0].run_id.as_str()), (2, "bad1"));
    let back = store
        .list_runs(&RunQuery { before: next.first_cursor.clone(), ..attention.clone() })
        .await
        .unwrap();
    assert_eq!(back.runs[0].run_id.as_str(), "bad2");
    // Read (opened, or the bell's Mark all read), it still needs attention.
    store.mark_notifications_read("alice", &NotificationSelection::All).await.unwrap();
    let page = store.list_runs(&attention).await.unwrap();
    assert_eq!(page.total, 2);
    // Dismissed, it does not; again is a no-op (which dismissal is kept: the next test).
    for via in [DismissVia::Dismiss, DismissVia::TryAgain] {
        store.dismiss_attention("alice", &["bad2".into()], via).await.unwrap();
    }
    let page = store.list_runs(&attention).await.unwrap();
    assert_eq!((page.total, page.runs[0].run_id.as_str()), (1, "bad1"));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_dismissal_is_the_users_own_durable_and_records_how() {
    use biffix_backend::store::notifications::DismissVia;
    let pool = notified_pg("dismiss_attention").await;
    let store = PgStore::new(pool.clone());
    let attention = |user: &str| RunQuery {
        submitter: Some(format!("user:{user}")),
        attention: Some(user.into()),
        ..Default::default()
    };
    let ids = |page: biffix_backend::store::runs::RunPage| {
        let mut ids: Vec<String> = page.runs.iter().map(|r| r.run_id.to_string()).collect();
        ids.sort();
        ids
    };
    // Alice names exactly bad1, plus bob's run, a succeeded run and an unknown one: only her own
    // failure is dismissed; bad2 (not named) stays, and bob's stays his.
    store
        .dismiss_attention(
            "alice",
            &["bad1".into(), "theirs".into(), "ok".into(), "nope".into()],
            DismissVia::Dismiss,
        )
        .await
        .unwrap();
    let alice = store.list_runs(&attention("alice")).await.unwrap();
    assert_eq!(ids(alice), ["bad2"]);
    let bob = store.list_runs(&attention("bob")).await.unwrap();
    assert_eq!(ids(bob), ["theirs"]);
    // Dismissing is not reading: the bell still has both failures unread.
    let list = store.list_notifications("alice", 20).await.unwrap();
    assert_eq!(list.unread_failed, 2);
    // A try-again of bad2 dismisses it, recorded as such, beside alice's own dismissal; a later
    // dismissal of either (by hand, or another try-again) keeps the first on record.
    store.dismiss_attention("alice", &["bad2".into()], DismissVia::TryAgain).await.unwrap();
    let first_at: Vec<(String, i64)> = sqlx::query_as(
        "SELECT notification_id, (EXTRACT(EPOCH FROM dismissed_at) * 1000000)::bigint \
         FROM notification_dismissals ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for via in [DismissVia::TryAgain, DismissVia::Dismiss] {
        store.dismiss_attention("alice", &["bad1".into(), "bad2".into()], via).await.unwrap();
    }
    let again_at: Vec<(String, i64)> = sqlx::query_as(
        "SELECT notification_id, (EXTRACT(EPOCH FROM dismissed_at) * 1000000)::bigint \
         FROM notification_dismissals ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(again_at, first_at);
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT user_sub, notification_id, via FROM notification_dismissals ORDER BY 2",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("alice".into(), "bad1:failed".into(), "dismiss".into()),
            ("alice".into(), "bad2:failed".into(), "try_again".into()),
        ]
    );
    // A refold rebuilds the notifications and leaves the dismissals standing: the failures are
    // back (so an empty Needs attention is the dismissals' doing, not missing notifications).
    biffix_db::projection::refold_runs(&pool, None, 100).await.unwrap();
    let list = store.list_notifications("alice", 20).await.unwrap();
    assert_eq!(list.unread_failed, 2, "the refold rebuilt both failures");
    assert!(list.items.iter().any(|n| n.id == "bad1:failed"));
    assert!(list.items.iter().any(|n| n.id == "bad2:failed"));
    let after: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT user_sub, notification_id, via FROM notification_dismissals ORDER BY 2",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(after, rows);
    assert!(store.list_runs(&attention("alice")).await.unwrap().runs.is_empty());
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn experiment_runs_of_counts_a_submitters_runs_and_those_needing_their_attention() {
    use biffix_backend::store::experiments::SubmitterRuns;
    use biffix_backend::store::notifications::{DismissVia, NotificationSelection};
    let pool = notified_pg("experiment_runs_of").await;
    // The fold puts runs in experiments from `experiment_set` events.
    let at = biffix_core::time::Timestamp::from_epoch_seconds;
    let events: Vec<_> = [("ok", "e1"), ("bad1", "e1"), ("theirs", "e1"), ("bad2", "e2")]
        .into_iter()
        .map(|(run, exp)| {
            biffix_core::event::experiment_set(&RunId::new(run), at(100.0), Some(exp), "alice")
        })
        .collect();
    biffix_db::log::append_and_advance(&pool, &events).await.unwrap();
    let store = PgStore::new(pool);
    let runs = |n, a| SubmitterRuns { runs: n, attention: a };

    let alice = store.experiment_runs_of("user:alice", "alice", None).await.unwrap();
    assert_eq!(alice.len(), 2, "{alice:?}");
    assert_eq!((alice["e1"], alice["e2"]), (runs(2, 1), runs(1, 1)));
    // Scoped to one experiment, the read holds that one alone.
    let only = store.experiment_runs_of("user:alice", "alice", Some("e2")).await.unwrap();
    assert_eq!(only.len(), 1, "{only:?}");
    assert_eq!(only["e2"], runs(1, 1));
    assert!(store
        .experiment_runs_of("user:alice", "alice", Some("e-none"))
        .await
        .unwrap()
        .is_empty());
    // Bob's run counts for Bob, with his own unread failure.
    let bob = store.experiment_runs_of("user:bob", "bob", None).await.unwrap();
    assert_eq!(bob.len(), 1);
    assert_eq!(bob["e1"], runs(1, 1));
    // Attention is the named user's: Alice's runs need nothing of Bob's.
    let crossed = store.experiment_runs_of("user:alice", "bob", None).await.unwrap();
    assert_eq!((crossed["e1"], crossed["e2"]), (runs(2, 0), runs(1, 0)));
    assert!(store.experiment_runs_of("user:nobody", "nobody", None).await.unwrap().is_empty());

    // Read, a failure still needs attention; dismissed, it no longer does, and the run still counts.
    store
        .mark_notifications_read("alice", &NotificationSelection::Ids(vec!["bad1:failed".into()]))
        .await
        .unwrap();
    let alice = store.experiment_runs_of("user:alice", "alice", None).await.unwrap();
    assert_eq!((alice["e1"], alice["e2"]), (runs(2, 1), runs(1, 1)));
    store.dismiss_attention("alice", &["bad1".into()], DismissVia::Dismiss).await.unwrap();
    let alice = store.experiment_runs_of("user:alice", "alice", None).await.unwrap();
    assert_eq!((alice["e1"], alice["e2"]), (runs(2, 0), runs(1, 1)));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_cancelled_run_s_detail_names_who_cancelled_it() {
    let store = notified_pool("cancelled_by_detail").await;
    let stop = store.get_run(&RunId::new("stop")).await.unwrap();
    // Alice cancelled her own run with no label recorded: her submitter label.
    assert_eq!(stop.cancelled_by.as_deref(), Some("alice@lab.example.org"));
    let bad = store.get_run(&RunId::new("bad1")).await.unwrap();
    assert_eq!(bad.cancelled_by, None);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn notifications_tied_on_time_order_bytewise_whatever_the_database_collation() {
    // `ab:c` < `abb` bytewise (':' < 'b'); a linguistic collation that ignores the colon orders
    // them the other way. Newest first means descending, so `abb` comes first, as the doubles say.
    let pool = migrated_pool("notification_tie_bytewise").await;
    assert_collation_disagrees(&pool, "abb", "ab:c").await;
    exec_all(
        &pool,
        &["INSERT INTO notifications (id, user_sub, run_id, kind, at) VALUES \
             ('ab:c:failed', 'alice', 'ab:c', 'failed', 5), \
             ('abb:failed', 'alice', 'abb', 'failed', 5)"],
    )
    .await;
    let store = PgStore::new(pool);
    let list = store.list_notifications("alice", 20).await.unwrap();
    let ids: Vec<&str> = list.items.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["abb:failed", "ab:c:failed"]);
}
