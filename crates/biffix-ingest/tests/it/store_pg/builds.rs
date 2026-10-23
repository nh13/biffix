//! The build projection, per subject, and its refold.

use super::*;

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn builds_fold_into_the_build_projection_per_subject() {
    use biffix_core::event::build_requested;
    use biffix_core::translate::from_aws_codebuild;
    use biffix_core::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};

    let pool = common::isolated_pool("builds_fold_into_the_build_projection").await;
    let codebuild = |secs: f64, arn: &str, status: CodeBuildStatus| {
        let digest = (status == CodeBuildStatus::Succeeded).then(|| format!("sha256:{arn}"));
        from_aws_codebuild(&CodeBuildEvent {
            ts: Timestamp::from_epoch_seconds(secs),
            build_arn: arn.into(),
            project: "biffix-t-runtime-build".into(),
            status,
            subject: "runtime:k".into(),
            outputs: digest.into_iter().map(|d| ("DIGEST".into(), d)).collect(),
            reason: None,
            logs_url: None,
        })
    };
    let requested = |secs: f64, arn: &str| {
        build_requested(
            Timestamp::from_epoch_seconds(secs),
            "runtime:k",
            arn,
            serde_json::json!({"version": "9.3.0"}),
        )
    };
    let row = || async {
        sqlx::query_as::<_, (String, String, serde_json::Value, Option<serde_json::Value>, i32, i64)>(
            "SELECT state, build_arn, outputs, request, builds, updated_at FROM build_projection \
             WHERE subject = 'runtime:k'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    ingest_batch(
        &pool,
        &[requested(1.0, "b1"), codebuild(2.0, "b1", CodeBuildStatus::InProgress)],
        &[],
    )
    .await
    .unwrap();
    let (state, arn, _, request, builds, _) = row().await;
    assert_eq!((state.as_str(), arn.as_str(), builds), ("running", "b1", 1));
    assert_eq!(request.unwrap()["version"], "9.3.0");

    // Finished in a later batch; a redelivered start changes nothing.
    ingest_batch(
        &pool,
        &[
            codebuild(9.0, "b1", CodeBuildStatus::Succeeded),
            codebuild(2.0, "b1", CodeBuildStatus::InProgress),
        ],
        &[],
    )
    .await
    .unwrap();
    let (state, _, outputs, _, _, updated) = row().await;
    assert_eq!(state, "succeeded");
    assert_eq!(outputs["DIGEST"], "sha256:b1");
    assert_eq!(updated, 9_000_000);

    // A rebuild: the subject follows its newest build.
    ingest_batch(&pool, &[requested(20.0, "b2")], &[]).await.unwrap();
    let (state, arn, outputs, _, builds, _) = row().await;
    assert_eq!((state.as_str(), arn.as_str(), builds), ("requested", "b2", 2));
    assert_eq!(outputs, serde_json::json!({}));
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_refold_replays_the_build_projection_and_drops_orphan_subjects() {
    use biffix_core::event::build_requested;

    let pool = common::isolated_pool("refold_replays_build_projection").await;
    ingest_batch(
        &pool,
        &[build_requested(
            Timestamp::from_epoch_seconds(1.0),
            "runtime:k",
            "b1",
            serde_json::json!({"version": "9.3.0"}),
        )],
        &[],
    )
    .await
    .unwrap();
    // Rows an older fold would have left: a wrong state for a live subject, and a subject the log
    // does not name at all.
    sqlx::query(
        "UPDATE build_projection SET state = 'failed', builds = 7 WHERE subject = 'runtime:k'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO build_projection (subject, state, build_arn, builds, updated_at) \
         VALUES ('orphan', 'succeeded', 'bx', 1, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    refold_runs(&pool, None, 100).await.unwrap();

    let rows: Vec<(String, String, i32)> =
        sqlx::query_as("SELECT subject, state, builds FROM build_projection ORDER BY subject")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, [("runtime:k".to_string(), "requested".to_string(), 1)]);
}
