//! Outputs: listing, filtering, paging, resume chains and cached S3 metadata.

use super::*;
use biffix_backend::store::outputs::OutputStore;
use biffix_backend::store::runs::RunReads;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn outputs_are_listed_filtered_paged_and_resolved_by_id() {
    use biffix_backend::store::outputs::OutputQuery;
    let pool = seeded_pool("outputs_are_listed_filtered_paged_and_resolved_by_id").await;
    for (run, uri, task, role) in [
        ("r1", "s3://b/p/a.bam", Some("align"), "output"),
        ("r1", "s3://b/p/a.log", Some("align"), "log"),
        ("r1", "s3://b/p/work/x", Some("align"), "intermediate"),
        ("r1", "s3://b/p/sub/c.tsv", None, "published"),
        ("r2", "s3://b/q/z", Some("t"), "output"),
    ] {
        sqlx::query(
            "INSERT INTO run_output (run_id, s3_uri, id, task_key, attempt, role, size) \
             VALUES ($1, $2, $3, $4, 1, $5, 7)",
        )
        .bind(run)
        .bind(uri)
        .bind(biffix_core::reducer::output_id(run, uri))
        .bind(task)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool);
    let r1 = RunId::new("r1");
    let q = |f: fn(&mut OutputQuery)| {
        let mut q = OutputQuery { limit: 100, ..Default::default() };
        f(&mut q);
        q
    };
    let uris = |page: &biffix_backend::store::outputs::OutputPage| {
        page.rows.iter().map(|r| r.s3_uri.clone()).collect::<Vec<_>>()
    };

    let all = store.list_outputs(&r1, &q(|_| {})).await.unwrap();
    assert_eq!(all.total, 3, "intermediates hidden by default");
    assert_eq!(uris(&all), ["s3://b/p/a.bam", "s3://b/p/a.log", "s3://b/p/sub/c.tsv"]);
    assert_eq!(all.rows[0].size, Some(7));
    assert_eq!(all.rows[0].attempt, Some(1));

    let intermediate =
        store.list_outputs(&r1, &q(|q| q.roles = Some(vec!["intermediate".into()]))).await.unwrap();
    assert_eq!(uris(&intermediate), ["s3://b/p/work/x"]);
    // `*` admits every role, intermediate included (the in-memory store agrees).
    let every = store
        .list_outputs(
            &r1,
            &q(|q| q.roles = Some(vec![biffix_backend::store::outputs::ALL_ROLES.into()])),
        )
        .await
        .unwrap();
    assert_eq!(every.total, 4);
    let task = store.list_outputs(&r1, &q(|q| q.task = Some("align".into()))).await.unwrap();
    // The task's own non-intermediate rows, not just as many.
    assert_eq!(task.total, 2);
    assert_eq!(uris(&task), ["s3://b/p/a.bam", "s3://b/p/a.log"]);
    let under =
        store.list_outputs(&r1, &q(|q| q.uri_prefix = Some("s3://b/p/sub/".into()))).await.unwrap();
    assert_eq!(uris(&under), ["s3://b/p/sub/c.tsv"]);

    // Keyset paging: a page of 2, then the rest after its last URI; total is the full count.
    let first = store.list_outputs(&r1, &q(|q| q.limit = 2)).await.unwrap();
    assert_eq!((first.rows.len(), first.total), (2, 3));
    let rest = store
        .list_outputs(
            &r1,
            &OutputQuery {
                after: Some(first.rows[1].s3_uri.clone()),
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(uris(&rest), ["s3://b/p/sub/c.tsv"]);
    assert_eq!(rest.total, 3, "a later page's total is still the full count");

    // Report candidates: LIKE patterns over published/output rows only, this run only.
    let alone = biffix_backend::store::outputs::OutputScope::run(&r1);
    let like = |patterns: &[&str]| {
        patterns.iter().map(std::string::ToString::to_string).collect::<Vec<_>>()
    };
    let found = store
        .outputs_like_in(&alone, &like(&["s3://b/p/%.tsv", "%.bam", "%/x"]), 10)
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|r| r.s3_uri.as_str()).collect::<Vec<_>>(),
        ["s3://b/p/a.bam", "s3://b/p/sub/c.tsv"],
        "the log and the intermediate are not report candidates"
    );
    assert!(store.outputs_like_in(&alone, &like(&["s3://b/q/%"]), 10).await.unwrap().is_empty());
    assert!(store.outputs_like_in(&alone, &[], 10).await.unwrap().is_empty());

    let id = biffix_core::reducer::output_id("r1", "s3://b/p/a.bam");
    assert_eq!(store.get_output_in(&alone, &id).await.unwrap().unwrap().s3_uri, "s3://b/p/a.bam");
    let r2_id = biffix_core::reducer::output_id("r2", "s3://b/q/z");
    assert_eq!(store.get_output_in(&alone, &r2_id).await.unwrap(), None);
    assert_eq!(
        store.output_at_in(&alone, "s3://b/p/sub/c.tsv").await.unwrap().map(|r| r.id),
        Some(biffix_core::reducer::output_id("r1", "s3://b/p/sub/c.tsv"))
    );
    assert_eq!(store.output_at_in(&alone, "s3://b/q/z").await.unwrap(), None);
}

/// A resumed run's outputs: the resume chain is read back (to a cycle, and to the hop cap), and an
/// [`OutputScope`] over it lists, matches and resolves the runs' rows as the in-memory test store
/// does: the run's own rows (wherever they are), the resumed runs' under the shared prefix only,
/// the nearest run's row for a URI, before any filter.
///
/// [`OutputScope`]: biffix_backend::store::outputs::OutputScope
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_resume_chain_and_its_outputs_are_read_across_its_runs() {
    use biffix_backend::store::outputs::{OutputQuery, OutputScope};
    let pool = migrated_pool("a_resume_chain_and_its_outputs_are_read_across_its_runs").await;
    for (run, from, prefix) in [
        ("r0", None, "s3://b/p/"),
        ("r1", Some("r0"), "s3://b/p/"),
        ("r2", Some("r1"), "s3://b/p/"),
        ("ca", Some("cb"), "s3://b/c/"),
        ("cb", Some("ca"), "s3://b/c/"),
        ("orphan", Some("gone"), "s3://b/o/"),
    ] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, output_prefix, resumed_from) \
             VALUES ($1, 'succeeded', $2, $3)",
        )
        .bind(run)
        .bind(prefix)
        .bind(from)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (run, uri, role) in [
        ("r0", "s3://b/p/a.bam", "output"),
        ("r0", "s3://b/p/b.tsv", "output"),
        ("r0", "s3://b/p/hidden.txt", "output"),
        ("r0", "s3://b/elsewhere/x", "output"),
        ("r1", "s3://b/p/b.tsv", "published"),
        ("r1", "s3://b/p/hidden.txt", "intermediate"),
        ("r2", "s3://b/p/c.txt", "output"),
        ("r2", "s3://b/elsewhere/mine", "output"),
    ] {
        sqlx::query("INSERT INTO run_output (run_id, s3_uri, id, role) VALUES ($1, $2, $3, $4)")
            .bind(run)
            .bind(uri)
            .bind(biffix_core::reducer::output_id(run, uri))
            .bind(role)
            .execute(&pool)
            .await
            .unwrap();
    }
    let store = PgStore::new(pool);
    // A resume says so in its summary (the list and the detail alike); its origin does not.
    let listed = store.list_runs(&RunQuery::default()).await.unwrap();
    let resumed = |id: &str| listed.runs.iter().find(|r| r.run_id.as_str() == id).unwrap().resumed;
    assert!(!resumed("r0"));
    assert!(resumed("r1") && resumed("orphan"));
    assert!(store.get_run(&RunId::new("r2")).await.unwrap().run.resumed);
    let ids = |origins: Vec<biffix_backend::store::runs::ResumeOrigin>| {
        origins
            .into_iter()
            .map(|o| (o.run_id.to_string(), o.output_prefix.unwrap()))
            .collect::<Vec<_>>()
    };
    let origins = |run: &str, hops| {
        let store = &store;
        let run = RunId::new(run);
        async move { ids(store.resume_origins(&run, hops).await.unwrap()) }
    };
    assert_eq!(
        origins("r2", 20).await,
        [("r1".to_string(), "s3://b/p/".to_string()), ("r0".to_string(), "s3://b/p/".to_string())]
    );
    assert_eq!(
        origins("r2", 1).await,
        [("r1".to_string(), "s3://b/p/".to_string())],
        "the hop cap keeps the nearest"
    );
    assert_eq!(
        origins("ca", 20).await,
        [("cb".to_string(), "s3://b/c/".to_string())],
        "a cycle stops at the run it started from"
    );
    assert!(origins("r0", 20).await.is_empty());
    assert!(origins("orphan", 20).await.is_empty(), "an unknown origin");
    assert!(origins("nope", 20).await.is_empty());

    let scope =
        OutputScope::resumed(&RunId::new("r2"), ["r1", "r0"].map(RunId::new).to_vec(), "s3://b/p/");
    let rows = |page: Vec<biffix_backend::store::outputs::OutputRow>| {
        page.into_iter().map(|r| (r.s3_uri, r.run_id.to_string())).collect::<Vec<_>>()
    };
    let own = |uri: &str, run: &str| (uri.to_string(), run.to_string());
    let page = store
        .list_outputs_in(&scope, &OutputQuery { limit: 100, ..Default::default() })
        .await
        .unwrap();
    // r1's intermediate hides r0's `hidden.txt` before the default filter drops it; r2's own
    // output outside the shared prefix is still its own, r0's is not.
    assert_eq!(
        rows(page.rows),
        [
            own("s3://b/elsewhere/mine", "r2"),
            own("s3://b/p/a.bam", "r0"),
            own("s3://b/p/b.tsv", "r1"),
            own("s3://b/p/c.txt", "r2")
        ]
    );
    assert_eq!(page.total, 4);
    let page = store
        .list_outputs_in(
            &scope,
            &OutputQuery { limit: 1, after: Some("s3://b/p/a.bam".into()), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(rows(page.rows), [own("s3://b/p/b.tsv", "r1")]);
    assert_eq!(page.total, 4);
    // A lone run reads only its own, as before.
    let alone = store
        .list_outputs(&RunId::new("r1"), &OutputQuery { limit: 100, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(rows(alone.rows), [own("s3://b/p/b.tsv", "r1")]);

    let like = store.outputs_like_in(&scope, &["s3://b/%".to_string()], 10).await.unwrap();
    assert_eq!(
        rows(like),
        [
            own("s3://b/elsewhere/mine", "r2"),
            own("s3://b/p/a.bam", "r0"),
            own("s3://b/p/b.tsv", "r1"),
            own("s3://b/p/c.txt", "r2")
        ]
    );

    let get = |run: &str, uri: &str| {
        let store = &store;
        let scope = &scope;
        let id = biffix_core::reducer::output_id(run, uri);
        async move { store.get_output_in(scope, &id).await.unwrap().map(|r| r.run_id.to_string()) }
    };
    assert_eq!(get("r0", "s3://b/p/a.bam").await.as_deref(), Some("r0"));
    assert_eq!(get("r1", "s3://b/p/b.tsv").await.as_deref(), Some("r1"));
    assert_eq!(get("r0", "s3://b/p/b.tsv").await, None, "hidden by r1's");
    assert_eq!(get("r0", "s3://b/elsewhere/x").await, None, "outside the prefix");
    assert_eq!(
        get("r2", "s3://b/elsewhere/mine").await.as_deref(),
        Some("r2"),
        "the run's own, outside the prefix"
    );
    // A lone run's scope has only its own rows.
    let lone = OutputScope::run(&RunId::new("r2"));
    let id = biffix_core::reducer::output_id("r0", "s3://b/p/a.bam");
    assert_eq!(store.get_output_in(&lone, &id).await.unwrap(), None);

    // By URI, in one read: the nearest run's row, of any role.
    let at = |scope: &OutputScope, uri: &str| {
        let store = &store;
        let (scope, uri) = (scope.clone(), uri.to_string());
        async move {
            store.output_at_in(&scope, &uri).await.unwrap().map(|r| (r.run_id.to_string(), r.id))
        }
    };
    let hit =
        |run: &str, uri: &str| Some((run.to_string(), biffix_core::reducer::output_id(run, uri)));
    assert_eq!(at(&scope, "s3://b/p/a.bam").await, hit("r0", "s3://b/p/a.bam"));
    assert_eq!(at(&scope, "s3://b/p/b.tsv").await, hit("r1", "s3://b/p/b.tsv"));
    assert_eq!(
        at(&scope, "s3://b/p/hidden.txt").await,
        hit("r1", "s3://b/p/hidden.txt"),
        "any role: r1's intermediate"
    );
    assert_eq!(at(&scope, "s3://b/elsewhere/mine").await, hit("r2", "s3://b/elsewhere/mine"));
    assert_eq!(at(&scope, "s3://b/elsewhere/x").await, None, "outside the prefix");
    assert_eq!(at(&scope, "s3://b/p/nope").await, None);
    assert_eq!(at(&lone, "s3://b/p/a.bam").await, None);
}

/// A lone run's output page keeps the ordered scan of `run_output_uri_bytes` that stops at the
/// page's `LIMIT` (no sort of every row the run has), and a resume chain's page reads only the
/// scope's runs' index entries, never the whole table. Sorting and sequential scans are switched
/// off so the plan shows what the query allows rather than what a tiny table makes cheapest.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn output_pages_are_planned_on_the_runs_uri_index() {
    use biffix_backend::pg::outputs::output_page_sql;
    use biffix_backend::store::outputs::OutputScope;
    let pool = migrated_pool("output_pages_are_planned_on_the_runs_uri_index").await;
    let mut conn = pool.acquire().await.unwrap();
    for set in ["SET enable_sort = off", "SET enable_seqscan = off"] {
        sqlx::query(set).execute(&mut *conn).await.unwrap();
    }
    async fn plan(
        conn: &mut sqlx::PgConnection,
        sql: String,
        runs: Vec<&str>,
        prefix: Option<&str>,
    ) -> String {
        let explain = format!("EXPLAIN (COSTS OFF) {sql}");
        let q = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(explain))
            .bind(runs)
            .bind(None::<String>)
            .bind(None::<Vec<String>>)
            .bind(None::<String>)
            .bind("")
            .bind(500_i64);
        let q = match prefix {
            Some(p) => q.bind(p),
            None => q,
        };
        q.fetch_all(conn).await.unwrap().join("\n")
    }
    let lone =
        plan(&mut conn, output_page_sql(&OutputScope::run(&RunId::new("r1"))), vec!["r1"], None)
            .await;
    assert!(
        lone.contains("Index Scan using run_output_uri_bytes") && !lone.contains("Sort"),
        "{lone}"
    );
    let chain = OutputScope::resumed(
        &RunId::new("r2"),
        vec![RunId::new("r1"), RunId::new("r0")],
        "s3://b/p/",
    );
    let resumed =
        plan(&mut conn, output_page_sql(&chain), vec!["r2", "r1", "r0"], Some("s3://b/p/")).await;
    assert!(!resumed.contains("Seq Scan"), "{resumed}");
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn cached_s3_metadata_is_joined_and_counted_as_checked() {
    use biffix_backend::store::outputs::{ObjectMeta, OutputQuery, MISSING_RECHECK_MICROS};
    let pool = seeded_pool("cached_s3_metadata_is_joined_and_counted_as_checked").await;
    for uri in [
        "s3://b/p/a.bam",
        "s3://b/p/b.bam",
        "s3://b/p/gone.txt",
        "s3://b/p/old-gone.txt",
        "s3://b/workflow-sources/v1/source.tar.gz",
        "s3://other/x",
    ] {
        sqlx::query(
            "INSERT INTO run_output (run_id, s3_uri, id, role, etag) \
             VALUES ('r1', $1, $2, 'output', 'produced')",
        )
        .bind(uri)
        .bind(biffix_core::reducer::output_id("r1", uri))
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool);
    let r1 = RunId::new("r1");
    let now = biffix_core::clock::epoch_micros();
    let query =
        OutputQuery { limit: 100, checkable_bucket: Some("b".into()), ..Default::default() };

    // Nothing checked: the three plain objects plus old-gone are pending; the staged source and
    // the foreign bucket are never checkable.
    let page = store.list_outputs(&r1, &query).await.unwrap();
    assert_eq!((page.total, page.unchecked), (6, 4));
    assert!(page.rows.iter().all(|r| r.meta.is_none()));
    assert_eq!(page.rows[0].etag.as_deref(), Some("produced"));

    let present = ObjectMeta {
        present: true,
        size: Some(42),
        etag: Some("e1".into()),
        last_modified: Some(1_700_000_000_000_000),
        checked_at: now,
    };
    let missing = |checked_at| ObjectMeta {
        present: false,
        size: None,
        etag: None,
        last_modified: None,
        checked_at,
    };
    store
        .put_object_meta(&[
            ("s3://b/p/a.bam".into(), present.clone()),
            ("s3://b/p/gone.txt".into(), missing(now)),
            ("s3://b/p/old-gone.txt".into(), missing(now - MISSING_RECHECK_MICROS - 1)),
        ])
        .await
        .unwrap();
    let page = store.list_outputs(&r1, &query).await.unwrap();
    // b.bam is unchecked, old-gone's "missing" is stale; a.bam and gone.txt are settled.
    assert_eq!(page.unchecked, 2);
    let meta_of = |page: &biffix_backend::store::outputs::OutputPage, uri: &str| {
        page.rows.iter().find(|r| r.s3_uri == uri).unwrap().meta.clone()
    };
    assert_eq!(meta_of(&page, "s3://b/p/a.bam"), Some(present.clone()));
    let id = biffix_core::reducer::output_id("r1", "s3://b/p/gone.txt");
    let gone = store
        .get_output_in(&biffix_backend::store::outputs::OutputScope::run(&r1), &id)
        .await
        .unwrap()
        .unwrap();
    assert!(!gone.meta.unwrap().present);

    // Replacing a result is an upsert: a.bam re-probed after it changed, and old-gone re-probed
    // (its stale "missing") and found after all.
    let changed = ObjectMeta { size: Some(43), etag: Some("e2".into()), ..present.clone() };
    let found = ObjectMeta { size: Some(1), ..present.clone() };
    store
        .put_object_meta(&[
            ("s3://b/p/a.bam".into(), changed.clone()),
            ("s3://b/p/old-gone.txt".into(), found.clone()),
        ])
        .await
        .unwrap();
    let page = store.list_outputs(&r1, &query).await.unwrap();
    assert_eq!(page.unchecked, 1, "only b.bam is left unchecked");
    assert_eq!(meta_of(&page, "s3://b/p/a.bam"), Some(changed.clone()));
    assert_eq!(meta_of(&page, "s3://b/p/old-gone.txt"), Some(found));

    // An older check written last (two overlapping loads, the earlier one's HEAD finishing after)
    // does not replace the newer result.
    store.put_object_meta(&[("s3://b/p/a.bam".into(), missing(now - 1))]).await.unwrap();
    let page = store.list_outputs(&r1, &query).await.unwrap();
    assert_eq!(meta_of(&page, "s3://b/p/a.bam"), Some(changed.clone()));

    // No checkable bucket ⇒ nothing counts as pending.
    let page =
        store.list_outputs(&r1, &OutputQuery { limit: 100, ..Default::default() }).await.unwrap();
    assert_eq!(page.unchecked, 0);
}
