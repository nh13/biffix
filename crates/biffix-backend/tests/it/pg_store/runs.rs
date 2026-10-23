//! Run reads: listing, filtering, searching and paging runs, run detail and events by kind.

use super::*;
use biffix_backend::store::event_log::EventLogReads;
use biffix_backend::store::outputs::OutputStore;
use biffix_backend::store::runs::RunReads;
use biffix_backend::store::tasks::TaskReads;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn lists_and_filters_runs() {
    let store = PgStore::new(seeded_pool("lists_and_filters_runs").await);
    let q = |filter| RunQuery { filter, ..Default::default() };
    let all = store.list_runs(&q(RunFilter::All)).await.unwrap();
    assert_eq!(all.total, 2);
    assert_eq!(all.runs.len(), 2);
    let active = store.list_runs(&q(RunFilter::Active)).await.unwrap();
    assert_eq!(active.runs.len(), 1);
    assert_eq!(active.runs[0].run_id.as_str(), "r1");
    let succeeded = store.list_runs(&q(RunFilter::Succeeded)).await.unwrap();
    assert_eq!(succeeded.runs.len(), 1);
    assert_eq!(succeeded.runs[0].run_id.as_str(), "r2");
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn reads_a_run_s_sample_facts() {
    let pool = seeded_pool("reads_a_run_s_sample_facts").await;
    exec_all(
        &pool,
        &[
            "UPDATE run_projection SET samples = '{\"ids\": [\"A\"]}', samples_total = 4, \
               samples_done = 2, samples_failed = 1, samples_declared = true WHERE run_id = 'r1'",
            "UPDATE task_projection SET sample = 'A', \"group\" = 'align' WHERE run_id = 'r1'",
        ],
    )
    .await;
    let store = PgStore::new(pool.clone());
    let (spec, tasks) = store.sample_facts(&RunId::new("r1")).await.unwrap();
    assert_eq!(spec.unwrap().ids, ["A"]);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].sample.as_deref(), Some("A"));
    assert_eq!(tasks[0].group.as_deref(), Some("align"));
    assert_eq!((tasks[0].state.as_str(), tasks[0].attempts), ("running", 2));
    let run = store.get_run(&RunId::new("r1")).await.unwrap().run;
    // Distinct values per column, so a swapped column fails.
    assert_eq!(
        run.samples,
        Some(biffix_api::RunSampleCounts { total: 4, done: 2, failed: 1, declared: true })
    );
    // The list read maps the same columns.
    let listed = store.list_runs(&RunQuery::default()).await.unwrap();
    let r1 = listed.runs.iter().find(|r| r.run_id.as_str() == "r1").unwrap();
    assert_eq!(r1.samples, run.samples);
    // An unknown run has neither.
    let (spec, tasks) = store.sample_facts(&RunId::new("nope")).await.unwrap();
    assert!(spec.is_none() && tasks.is_empty());
    // The sample ids: the sheet's, then the other attributed ones (each once), as the default
    // over `sample_facts` reads them.
    assert_eq!(store.run_sample_ids(&RunId::new("r1")).await.unwrap(), ["A"]);
    exec_all(
        &pool,
        &["UPDATE run_projection SET samples = '{\"ids\": [\"B\", \"B\"]}' WHERE run_id = 'r1'"],
    )
    .await;
    assert_eq!(store.run_sample_ids(&RunId::new("r1")).await.unwrap(), ["B", "A"]);
    assert!(store.run_sample_ids(&RunId::new("nope")).await.unwrap().is_empty());
    // The stored sample by task key: only the keys asked for that have one.
    let keys = ["align:sample=A".to_string(), "nope".to_string()];
    let samples = store.task_samples(&RunId::new("r1"), &keys).await.unwrap();
    assert_eq!(
        samples.into_iter().collect::<Vec<_>>(),
        [("align:sample=A".to_string(), "A".to_string())]
    );
    assert!(store.task_samples(&RunId::new("r1"), &keys[1..]).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn lists_one_submitters_runs() {
    let store = PgStore::new(seeded_pool("lists_one_submitters_runs").await);
    let r1_submitter = store.get_run(&RunId::new("r1")).await.unwrap().run.submitter;
    let submitter = r1_submitter.expect("the seed gives r1 a submitter");
    let mine = RunQuery { submitter: Some(submitter), ..Default::default() };
    let page = store.list_runs(&mine).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.runs[0].run_id.as_str(), "r1");
    // With a cursor too (the submitter is $3; the keyset parameters follow it).
    let next = RunQuery { after: page.last_cursor.clone(), ..mine.clone() };
    assert!(store.list_runs(&next).await.unwrap().runs.is_empty());
    let nobody = RunQuery { submitter: Some("user:nobody".into()), ..Default::default() };
    assert_eq!(store.list_runs(&nobody).await.unwrap().total, 0);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn gets_run_detail_with_tasks() {
    let store = PgStore::new(seeded_pool("gets_run_detail_with_tasks").await);
    let d = store.get_run(&RunId::new("r1")).await.unwrap();
    assert_eq!(d.run.state, "running");
    assert_eq!(d.run.completed, Some(1));
    assert_eq!(d.run.submitter.as_deref(), Some("user:alice"));
    assert_eq!(
        d.run.workflow.as_ref().map(biffix_core::ids::WorkflowVersionId::as_str),
        Some("nf-core/rnaseq @ 3.14.0")
    );
    assert!(d.run.submitted_at.is_some());
    assert_eq!(d.tasks.len(), 1);
    assert_eq!(d.tasks[0].task_key.as_str(), "align:sample=A");
    assert_eq!(d.tasks[0].attempts, 2);

    assert!(matches!(
        store.get_run(&RunId::new("nope")).await,
        Err(biffix_backend::store::StoreError::NotFound)
    ));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_state_is_a_point_lookup_of_the_derived_state() {
    // Backs `POST /events`' revocation-by-recheck: a single-column read of a run's derived state,
    // None when the run isn't projected.
    let store = PgStore::new(seeded_pool("run_state_is_a_point_lookup").await);
    assert_eq!(store.run_state(&RunId::new("r1")).await.unwrap().as_deref(), Some("running"));
    assert_eq!(store.run_state(&RunId::new("r2")).await.unwrap().as_deref(), Some("succeeded"));
    assert_eq!(store.run_state(&RunId::new("nope")).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn an_active_run_in_a_prefix_is_found_and_ended_ones_are_not() {
    let pool = seeded_pool("active_run_in_prefix").await;
    let prefix = "s3://b/biffix/runs/r0/";
    for (run_id, state, output_prefix) in [
        ("p0", "failed", prefix),
        ("p1", "succeeded", prefix),
        ("p2", "cancelled", prefix),
        ("p3", "running", "s3://b/biffix/runs/r0"),
        ("p4", "running", "s3://b/other/"),
    ] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, output_prefix) VALUES ($1, $2, $3)",
        )
        .bind(run_id)
        .bind(state)
        .bind(output_prefix)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool.clone());
    // Only ended runs (and a prefix spelled otherwise) use it. The match is exact: every recorded
    // prefix is normalized to end in `/` before it is written (`submit::output_prefix_override`,
    // and the per-run default of `runs::output_prefix::effective_output_prefix`), so the slash-less p3 is a row no
    // writer produces, not another spelling of the prefix a run is using.
    assert_eq!(store.active_run_in_prefix(prefix).await.unwrap(), None);
    for (run_id, state) in [("p5", "submitted"), ("p6", "running")] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, output_prefix) VALUES ($1, $2, $3)",
        )
        .bind(run_id)
        .bind(state)
        .bind(prefix)
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(store.active_run_in_prefix(prefix).await.unwrap(), Some(RunId::new("p5")));
    assert_eq!(store.active_run_in_prefix("s3://b/nothing/").await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_engine_reads_the_joined_engine() {
    // Backs the ingest engine-dispatch: the run's engine comes from its workflow version, is None
    // when the version is not joined, and None for an unprojected run.
    let pool = seeded_pool("run_engine_reads_the_joined_engine").await;
    exec_all(
        &pool,
        &[
            "INSERT INTO workflow_versions (registration_inputs, registration_inputs_hash, \
             runtime_key, runtime_recipe, \
             id, name, project, git_sha, source_content_hash, \
             image_digest, engine, engine_version, entrypoint, source, form_schema, queue_classes, \
             created_at) \
             VALUES ('{}'::jsonb, '', '', '{}'::jsonb, \
             'wf-nf', 'w', 'p', 'abc', 'h', 'sha256:x', 'nextflow', '25.10', 'main.nf', \
             '{}'::jsonb, '{}'::jsonb, '{}'::text[], 1759500000000000)",
            "INSERT INTO run_projection (run_id, state, workflow_version) \
             VALUES ('rnf','running','wf-nf')",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    assert_eq!(store.run_engine(&RunId::new("rnf")).await.unwrap().as_deref(), Some("nextflow"));
    // r2 has no workflow_version → engine unknown → None (caller defaults to Snakemake).
    assert_eq!(store.run_engine(&RunId::new("r2")).await.unwrap(), None);
    assert_eq!(store.run_engine(&RunId::new("nope")).await.unwrap(), None);
    // The run detail reads when its version was registered from the same join; a run whose
    // version is not joined has none.
    assert_eq!(
        store.get_run(&RunId::new("rnf")).await.unwrap().workflow_registered_at_micros,
        Some(1_759_500_000_000_000)
    );
    assert_eq!(store.get_run(&RunId::new("r2")).await.unwrap().workflow_registered_at_micros, None);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn keyset_paging_covers_all_rows_in_order_without_gaps() {
    // Seeded: r2 (no submitted_at, so it sorts first) and r1 (submitted 1700000000000000). A few
    // more rows submitted earlier, with distinct times, so the list has a known total order.
    let pool = seeded_pool("keyset_paging_covers_all_rows_in_order_without_gaps").await;
    for i in 0..3_i64 {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, submitted_at) VALUES ($1, 'succeeded', $2)",
        )
        .bind(format!("k{i}"))
        .bind(1_600_000_000_000_000 - i)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool);
    let ids = |p: &biffix_backend::store::runs::RunPage| -> Vec<String> {
        p.runs.iter().map(|r| r.run_id.as_str().to_string()).collect()
    };
    let q = |after: Option<String>, before: Option<String>| RunQuery {
        filter: RunFilter::All,
        limit: 2,
        after,
        before,
        ..Default::default()
    };

    // Ground truth: one big page (same ordering the keyset must reproduce).
    let all = store
        .list_runs(&RunQuery { filter: RunFilter::All, limit: 100, ..Default::default() })
        .await
        .unwrap();
    let all_ids = ids(&all);
    assert_eq!(all_ids, ["r2", "r1", "k0", "k1", "k2"]);

    // Page forward at limit=2 via last_cursor; collect and compare to the single-page order.
    let mut collected = Vec::new();
    let mut after = None;
    let mut page2_first_cursor = None;
    loop {
        let p = store.list_runs(&q(after.clone(), None)).await.unwrap();
        if p.runs.is_empty() {
            break;
        }
        if collected.len() == 2 {
            page2_first_cursor = p.first_cursor.clone(); // remember page 2's newer edge
        }
        collected.extend(ids(&p));
        if p.runs.len() < 2 {
            break;
        }
        after = p.last_cursor;
    }
    assert_eq!(
        collected, all_ids,
        "forward keyset paging reproduces the single-page order with no gaps or duplicates"
    );

    // Page back from page 2 via its first_cursor → the first page again (in the same DESC order).
    let prev = store.list_runs(&q(None, page2_first_cursor)).await.unwrap();
    assert_eq!(ids(&prev), all_ids[0..2], "before-cursor returns the previous page in order");
}

/// A stored value of the wrong shape is corrupt data (Serialization), not an outage.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn an_undecodable_anchor_is_a_serialization_error() {
    use biffix_backend::store::StoreError;
    let pool = migrated_pool("undecodable_anchor").await;
    exec_all(
        &pool,
        &[
            "INSERT INTO run_events (dedup_key, ts_micros, source, kind, run_id, schema_version, \
           payload) VALUES ('bad', 1, 'biffix-control-plane', 'run_submitted', 'r', 1, '{\"x\": 1}')",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let err = store.run_anchor(&RunId::new("r")).await.unwrap_err();
    assert!(matches!(err, StoreError::Serialization(_)), "{err:?}");
}

/// The ids of a run page, in order.
fn page_ids(page: &biffix_backend::store::runs::RunPage) -> Vec<String> {
    page.runs.iter().map(|r| r.run_id.as_str().to_string()).collect()
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn an_undecodable_cursor_serves_the_first_page_in_order() {
    use base64::Engine;
    let token = |raw: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let store = PgStore::new(seeded_pool("undecodable_cursor_first_page").await);
    let first = store.list_runs(&RunQuery::default()).await.unwrap();
    assert_eq!(page_ids(&first), ["r2", "r1"]);
    // Garbage, a key that is not an integer, and the timestamp-text cursor earlier versions issued
    // (a digits-only one used to reach the `::timestamptz` cast and fail as a 503).
    for bad in [
        "garbage".to_string(),
        token("1-1|r1"),
        token("12345 |r1"),
        token("2026-09-19 04:12:31.123456+00|r1"),
    ] {
        for (after, before) in [(Some(bad.clone()), None), (None, Some(bad.clone()))] {
            let page =
                store.list_runs(&RunQuery { after, before, ..Default::default() }).await.unwrap();
            assert_eq!(page, first, "{bad:?} is no cursor: the first page, not reversed");
        }
    }
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_run_that_advances_mid_walk_is_listed_exactly_once() {
    let pool = migrated_pool("run_advances_mid_walk").await;
    for (run, submitted) in [("new", 2_000_i64), ("old", 1_000)] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, submitted_at) VALUES ($1, 'running', $2)",
        )
        .bind(run)
        .bind(submitted)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool.clone());
    let q = |after| RunQuery { limit: 1, after, ..Default::default() };
    let page1 = store.list_runs(&q(None)).await.unwrap();
    assert_eq!(page_ids(&page1), ["new"]);
    // The older run advances between the two page reads: the fold rewrites its row, moving its
    // write time past the newer run's.
    sqlx::query(
        "UPDATE run_projection SET state = 'succeeded', updated_at = now() + interval '1 hour' \
         WHERE run_id = 'old'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let page2 = store.list_runs(&q(page1.last_cursor.clone())).await.unwrap();
    assert_eq!(page_ids(&page2), ["old"], "the advanced run is not skipped");
    let page3 = store.list_runs(&q(page2.last_cursor.clone())).await.unwrap();
    assert!(page3.runs.is_empty(), "nor repeated: {:?}", page_ids(&page3));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn search_matches_each_documented_field_literally_and_workflow_scopes_exactly() {
    let pool = migrated_pool("search_fields_and_workflow_scope").await;
    // v2's form carries a title (frozen untrimmed, as an older form may be).
    for (id, name, project, engine, form) in [
        ("v1", "rnaseq", "genomics", "snakemake", serde_json::json!({})),
        ("v2", "sarek", "oncology", "nextflow", serde_json::json!({"title": "\tTumour calling\n"})),
    ] {
        sqlx::query(
            "INSERT INTO workflow_versions (registration_inputs, registration_inputs_hash, \
             runtime_key, runtime_recipe, \
             id, name, project, git_sha, source_content_hash, \
             image_digest, engine, engine_version, entrypoint, source, form_schema, queue_classes, \
             created_at) \
             VALUES ('{}'::jsonb, '', '', '{}'::jsonb, \
             $1, $2, $3, 'abc', $1, 'sha256:x', $4, '1', 'main', \
             '{}'::jsonb, $5, '{}'::text[], 0)",
        )
        .bind(id)
        .bind(name)
        .bind(project)
        .bind(engine)
        .bind(form)
        .execute(&pool)
        .await
        .unwrap();
    }
    // (run id, run name, submitter, submitter label, workflow version)
    for (run, run_name, submitter, label, version) in [
        ("run-a_b", "plain", "user:alice", "Alice", "v1"),
        ("run-axb", "100% done", "user:bob", "Bob", "v1"),
        ("run-c", "back\\slash", "user:carol", "Carol Q", "v2"),
    ] {
        sqlx::query(
            "INSERT INTO run_projection \
               (run_id, state, run_name, submitter, submitter_label, workflow_version) \
             VALUES ($1, 'running', $2, $3, $4, $5)",
        )
        .bind(run)
        .bind(run_name)
        .bind(submitter)
        .bind(label)
        .bind(version)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool);
    let search = |q: Option<&str>, workflow: Option<&str>| RunQuery {
        q: q.map(str::to_string),
        workflow: workflow.map(str::to_string),
        ..Default::default()
    };
    // Runs with no submit time tie on the sort key, so they list by run id descending, bytewise.
    let cases: [(Option<&str>, Option<&str>, &[&str]); 16] = [
        // `_`, `%` and `\` are literal, not wildcards or escapes: a wildcard `%` would have `a%b`
        // match run-a_b and run-axb.
        (Some("a_b"), None, &["run-a_b"]),
        (Some("100%"), None, &["run-axb"]),
        (Some("a%b"), None, &[]),
        (Some("k\\s"), None, &["run-c"]),
        // Each documented field, case-insensitively: run name, workflow name, version id,
        // submitter, submitter label, project, engine.
        (Some("PLAIN"), None, &["run-a_b"]),
        (Some("sarek"), None, &["run-c"]),
        (Some("rnaseq"), None, &["run-axb", "run-a_b"]),
        (Some("v2"), None, &["run-c"]),
        (Some("user:bob"), None, &["run-axb"]),
        (Some("carol q"), None, &["run-c"]),
        (Some("oncology"), None, &["run-c"]),
        (Some("snakemake"), None, &["run-axb", "run-a_b"]),
        // The pipeline's title (its form's `title`).
        (Some("TUMOUR"), None, &["run-c"]),
        // The whole term must sit inside one field.
        (Some("alice snakemake"), None, &[]),
        // The exact version scope, alone and with a search.
        (None, Some("v1"), &["run-axb", "run-a_b"]),
        (Some("bob"), Some("v2"), &[]),
    ];
    for (q, workflow, want) in cases {
        let page = store.list_runs(&search(q, workflow)).await.unwrap();
        assert_eq!(page_ids(&page), want, "q={q:?} workflow={workflow:?}");
        assert_eq!(page.total, want.len() as i64, "q={q:?} workflow={workflow:?}");
    }
    // The title is read trimmed of all whitespace, as a version's `title` is, and absent when
    // the form has none.
    let page = store.list_runs(&search(None, None)).await.unwrap();
    let title = |id: &str| {
        page.runs.iter().find(|r| r.run_id.as_str() == id).unwrap().workflow_title.clone()
    };
    assert_eq!(title("run-c").as_deref(), Some("Tumour calling"));
    assert_eq!(title("run-a_b"), None);
}

/// `run_event` reads a run's control-plane singleton by kind; `run_anchor` is the `run_submitted`
/// one. A preparing run folds to `preparing_images` and counts as active.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_event_reads_by_kind() {
    use biffix_core::event::{Kind, RunPreparing, SubmitImage};
    use biffix_core::time::Timestamp;
    let pool = migrated_pool("run_event_by_kind").await;
    let run = RunId::new("r1");
    let preparing = biffix_core::event::run_preparing(
        &run,
        Timestamp::from_micros(1_000_000),
        RunPreparing {
            workflow_version_id: "v1".into(),
            params: serde_json::json!({}),
            input_uris: vec![],
            submitter: None,
            submitter_label: None,
            run_name: None,
            output_prefix: Some("s3://b/runs/r1/".into()),
            resumed_from: None,
            images: vec![SubmitImage {
                declared_ref: "repo:1".into(),
                source_ref: "repo:1@sha256:a".into(),
                content_key: "k".into(),
            }],
            experiment_id: None,
        },
    );
    biffix_db::log::append_and_advance(&pool, std::slice::from_ref(&preparing)).await.unwrap();
    let store = PgStore::new(pool.clone());

    assert_eq!(store.run_event(&run, &Kind::RunPreparing).await.unwrap(), Some(preparing));
    assert!(store.run_anchor(&run).await.unwrap().is_none());
    assert!(store.run_event(&run, &Kind::RunFailed).await.unwrap().is_none());
    assert_eq!(store.run_state(&run).await.unwrap().as_deref(), Some("preparing_images"));
    let active = store
        .list_runs(&RunQuery { filter: RunFilter::Active, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(active.runs.len(), 1);
    assert_eq!(store.active_run_in_prefix("s3://b/runs/r1/").await.unwrap(), Some(run.clone()));

    let failed = biffix_core::event::run_failed(
        &run,
        Timestamp::from_micros(2_000_000),
        biffix_core::event::FAILURE_STAGE_IMAGE_PREPARATION,
        "no image",
    );
    biffix_db::log::append_and_advance(&pool, std::slice::from_ref(&failed)).await.unwrap();
    assert_eq!(store.run_event(&run, &Kind::RunFailed).await.unwrap(), Some(failed.clone()));
    assert_eq!(store.run_state(&run).await.unwrap().as_deref(), Some("failed"));
    // The earliest of the kind wins: a later control-plane `run_failed` does not replace it. Nor
    // does the engine's own `run_failed`, even timed before it (the exit replay delivers it late):
    // it is not the control plane's, and its reason (none) would hide the control plane's.
    let later = biffix_core::event::run_failed(
        &run,
        Timestamp::from_micros(3_000_000),
        biffix_core::event::FAILURE_STAGE_IMAGE_PREPARATION,
        "another reason",
    );
    let engine = biffix_core::translate::from_engine_value(
        biffix_core::source::EventSource::Snakemake,
        serde_json::json!({"schema_version": 1, "ts": 1.5, "run_id": "r1", "kind": "run_failed"}),
    )
    .unwrap();
    biffix_db::log::append_and_advance(&pool, &[later, engine]).await.unwrap();
    assert_eq!(store.run_event(&run, &Kind::RunFailed).await.unwrap(), Some(failed));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_detail_reads_the_coordinator_s_stop() {
    let pool = seeded_pool("run_detail_reads_the_coordinator_s_stop").await;
    let store = PgStore::new(pool.clone());
    let d = store.get_run(&RunId::new("r1")).await.unwrap();
    assert_eq!(d.coordinator_stop, Default::default(), "nothing recorded");
    exec_all(
        &pool,
        &["UPDATE run_projection SET coordinator_exit_code = 137, \
             coordinator_status_reason = 'OutOfMemoryError: Container killed', \
             coordinator_termination_category = 'oom' WHERE run_id = 'r1'"],
    )
    .await;
    let stop = store.get_run(&RunId::new("r1")).await.unwrap().coordinator_stop;
    assert_eq!(stop.exit_code, Some(137));
    assert_eq!(stop.status_reason.as_deref(), Some("OutOfMemoryError: Container killed"));
    assert_eq!(stop.termination_category.as_deref(), Some("oom"));
}

/// A value Postgres refuses as data (a NUL in text) is not an outage: the caller's, never a
/// retryable error.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_value_postgres_refuses_is_not_read_as_an_outage() {
    let store = PgStore::new(seeded_pool("a_value_postgres_refuses").await);
    let err = store
        .list_runs(&RunQuery { q: Some("a\0b".into()), ..Default::default() })
        .await
        .unwrap_err();
    assert!(matches!(err, biffix_backend::store::StoreError::Rejected(_)), "{err:?}");
}

/// Runs that tie on the list's sort key (no submit time) list by run id descending, bytewise, as
/// the in-memory store orders them and the keyset cursor assumes: `x_b` before `xB`, which the
/// harness collation orders the other way.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn tied_runs_list_by_id_bytewise_whatever_the_database_collation() {
    let pool = migrated_pool("list_runs_tie_bytewise").await;
    assert_collation_disagrees(&pool, "x_b", "xB").await;
    exec_all(
        &pool,
        &["INSERT INTO run_projection (run_id, state) VALUES ('xB', 'running'), ('x_b', 'running')"],
    )
    .await;
    let store = PgStore::new(pool);
    let page = store.list_runs(&RunQuery::default()).await.unwrap();
    let ids: Vec<&str> = page.runs.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(ids, ["x_b", "xB"]);
}
