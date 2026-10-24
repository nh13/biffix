//! Round-trip tests for the wire contract: `deserialize(serialize(x)) == x`.
//!
//! The load-bearing case is the *minimal* variant of each type — every optional field `None`/empty,
//! so it is dropped by `skip_serializing_if` on the way out and must be supplied by `#[serde(default)]`
//! on the way back in. A `skip_serializing_if` that ever ships without its matching default makes the
//! minimal round-trip of a type covered here fail, which is exactly the drift these shared types
//! exist to prevent. The cases cover the requests and the responses the CLI and the UI read; a new
//! such type needs its own.

use std::fmt::Debug;

use biffix_api::*;
use biffix_core::ids::{CoordinatorJobId, RunId, TaskKey, WorkflowVersionId};
use proptest::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;

fn rt<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: T) {
    let json = serde_json::to_string(&value).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(value, back, "round-trip mismatch via {json}");
}

#[test]
fn oauth_client_config_round_trips() {
    rt(OAuthClientConfig {
        authorization_endpoint: "https://auth".into(),
        token_endpoint: "https://token".into(),
        client_id: "web".into(),
        redirect_uri: "https://app/cb".into(),
        logout_endpoint: "https://logout".into(),
        logout_uri: "https://app".into(),
        scopes: vec!["openid".into(), "email".into()],
        cli_client_id: None,
        cli_redirect_uris: vec![],
        user_pool_id: None,
        region: None,
    });
    rt(OAuthClientConfig {
        authorization_endpoint: "https://auth".into(),
        token_endpoint: "https://token".into(),
        client_id: "web".into(),
        redirect_uri: "https://app/cb".into(),
        logout_endpoint: "https://logout".into(),
        logout_uri: "https://app".into(),
        scopes: vec!["openid".into()],
        cli_client_id: Some("cli".into()),
        cli_redirect_uris: vec!["http://127.0.0.1:8765/cb".into()],
        user_pool_id: Some("us-east-1_abc".into()),
        region: Some("us-east-1".into()),
    });
}

#[test]
fn status_report_round_trips() {
    for status in [HealthStatus::Ok, HealthStatus::Degraded, HealthStatus::Down] {
        rt(status);
    }
    rt(StatusReport {
        status: HealthStatus::Degraded,
        components: vec![
            ComponentHealth { name: "database".into(), status: HealthStatus::Ok, detail: None },
            ComponentHealth {
                name: "reconciler".into(),
                status: HealthStatus::Degraded,
                detail: Some("stale sample".into()),
            },
        ],
    });
}

#[test]
fn status_history_round_trips() {
    rt(StatusHistory {
        bucket_secs: 300,
        window_secs: 3_600,
        expected_per_bucket: 1,
        grace_secs: 300,
        components: vec![ComponentHistory {
            name: "alarms".into(),
            buckets: vec![],
            carried_in: None,
        }],
    });
    rt(StatusHistory {
        bucket_secs: 86_400,
        window_secs: 7_776_000,
        expected_per_bucket: 288,
        grace_secs: 300,
        components: vec![ComponentHistory {
            name: "reconciler".into(),
            buckets: vec![UptimeBucket {
                t: 1_790_000_000,
                ok: 280,
                degraded: 6,
                down: 2,
                covered_until: 1_790_086_500,
                lapsed: true,
                last: HealthStatus::Ok,
            }],
            carried_in: Some(CarriedCoverage {
                covered_until: 1_790_000_300,
                last: HealthStatus::Down,
            }),
        }],
    });
}

#[test]
fn settings_round_trip() {
    rt(SiteSettings::default());
    rt(SiteSettings {
        title: Some("Lab".into()),
        logo_data_url: Some("data:image/png;base64,AAAA".into()),
        support: Some(SupportContact {
            name: "Bioinformatics core".into(),
            email: "core@lab.example.org".into(),
            url: Some("https://lab.example.org/help".into()),
        }),
    });
    // Each of an update's three per-field states survives: absent (keep), null (clear), a value.
    rt(SiteSettingsUpdateRequest::default());
    rt(SiteSettingsUpdateRequest {
        title: Some(None),
        logo_data_url: Some(Some("data:image/png;base64,AAAA".into())),
        support: Some(None),
    });
    rt(AlertRecipientList {
        configured: true,
        recipients: vec![AlertRecipient {
            email: "ops@example.org".into(),
            status: "pending".into(),
        }],
    });
    rt(AlertRecipientAddRequest { email: "ops@example.org".into() });
    rt(AlertRecipientRemoveRequest { email: "ops@example.org".into() });
}

#[test]
fn engine_support_round_trips() {
    for range in biffix_workflow::engines::SUPPORTED {
        rt(EngineSupport::from(*range));
    }
}

#[test]
fn register_request_round_trips() {
    rt(RegisterRequest {
        name: "wf".into(),
        project: "proj".into(),
        source: serde_json::json!({"kind": "git", "repo": "r", "git_ref": "main"}),
        git_sha: "abc".into(),
        source_content_hash: "def".into(),
        image: Some("sha256:x".into()),
        engine: None,
        engine_version: "8".into(),
        entrypoint: "Snakefile".into(),
        target: None,
        form_schema: serde_json::json!({"fields": [], "warnings": []}),
        queue_classes: None,
        about_markdown: Some("# About\n\nAligns reads.".into()),
        registration_inputs: RegistrationInputs {
            source_mode: "git".into(),
            cli_version: "0.0.0".into(),
            overrides: RegistrationOverrides::default(),
            reregistered_from: None,
        },
        images: Default::default(),
        nextflow_plugins: Vec::new(),
    });
    rt(RegisterRequest {
        name: "wf".into(),
        project: "proj".into(),
        source: serde_json::json!({"kind": "s3", "uri": "s3://b/workflow-sources/x/src.tar.gz"}),
        git_sha: "abc".into(),
        source_content_hash: "def".into(),
        image: None,
        engine: Some("nextflow".into()),
        engine_version: "25.10.4".into(),
        entrypoint: "main.nf".into(),
        target: None,
        form_schema: serde_json::json!({"fields": [], "warnings": []}),
        queue_classes: Some(vec!["cpu-spot".into()]),
        about_markdown: None,
        registration_inputs: RegistrationInputs {
            source_mode: "s3".into(),
            cli_version: "0.0.0".into(),
            overrides: RegistrationOverrides {
                engine_version: Some("25.10.4".into()),
                entrypoint: None,
                image: None,
                queue_classes: vec!["cpu-spot".into()],
            },
            reregistered_from: Some("abc:def:0".into()),
        },
        images: std::collections::BTreeMap::from([
            (
                "fastqc".to_string(),
                ImageRequest {
                    reference: Some("quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0".into()),
                    build: None,
                    runtime: None,
                },
            ),
            (
                "align".to_string(),
                ImageRequest {
                    reference: None,
                    build: Some(biffix_api::ImageBuildRequest {
                        kind: "dockerfile".into(),
                        context: "docker/align".into(),
                        dockerfile: Some("Dockerfile".into()),
                        context_uri: "s3://b/workflow-sources/v/contexts/t.tar.gz".into(),
                        context_sha256: "0".repeat(64),
                        tree_hash: "t".into(),
                        archs: vec!["amd64".into(), "arm64".into()],
                    }),
                    runtime: None,
                },
            ),
        ]),
        nextflow_plugins: vec![biffix_api::NextflowPluginRequest {
            id: "nf-schema".into(),
            version: "2.7.2".into(),
            sha256: "0".repeat(64),
        }],
    });
    // The minimal inputs: every override absent.
    rt(RegistrationInputs {
        source_mode: "git".into(),
        cli_version: "0.0.0".into(),
        overrides: RegistrationOverrides::default(),
        reregistered_from: None,
    });
}

#[test]
fn registered_version_views_round_trip() {
    rt(WorkflowVersionSummary {
        id: WorkflowVersionId::new("abc:def"),
        name: "wf".into(),
        project: "proj".into(),
        image_digest: "sha256:x".into(),
        image_repository: None,
        image_registry_id: None,
        image_region: None,
        engine: "snakemake".into(),
        engine_version: "8".into(),
        git_sha: "abc".into(),
        queue_classes: vec![],
        created_by: None,
        created_by_label: None,
        created_at_micros: 0,
        run_count: 0,
        title: Some("Read alignment".into()),
        summary: Some("Aligns reads.".into()),
        state: Some("pending".into()),
        catalog: Some(CatalogEntry {
            description: Some("Aligns reads.".into()),
            tags: vec!["DNA".into()],
            verified: true,
            contact: None,
            results: vec!["QC report".into()],
            inputs: vec!["Samples (one row per sample)".into()],
        }),
        last_run_at: Some(1.5),
        hidden: true,
        release: VersionRelease {
            released: true,
            at_micros: Some(9),
            by: Some("u2".into()),
            by_label: Some("u2@lab.example.org".into()),
        },
        typical: Some(TypicalRun { hours_median: 2.0, usd_median: None, runs: 3 }),
    });
    rt(WorkflowListingUpdateRequest {
        project: "proj".into(),
        name: "team/wf".into(),
        hidden: true,
    });
    rt(WorkflowListing {
        project: "proj".into(),
        name: "team/wf".into(),
        hidden: false,
        updated_at_micros: 7,
        updated_by: Some("u1".into()),
    });
    rt(WorkflowReleaseUpdateRequest { released: true });
    let history = vec![
        WorkflowReleaseChange {
            released: true,
            at_micros: 9,
            by: Some("u2".into()),
            by_label: Some("u2@lab.example.org".into()),
        },
        WorkflowReleaseChange { released: false, at_micros: 5, by: None, by_label: None },
    ];
    let release = WorkflowRelease::of(&"abc:def:0".into(), history.clone(), true);
    assert!(release.release.released);
    assert_eq!(release.release.at_micros, Some(9));
    assert_eq!(release.release.by.as_deref(), Some("u2"));
    rt(release);
    // The newest change un-released it: a developer version, its history kept.
    let unreleased = WorkflowRelease::of(&"abc:def:0".into(), history[1..].to_vec(), false);
    assert_eq!(unreleased.release, VersionRelease::default());
    // A developer version's release serializes as just `released: false`.
    assert_eq!(
        serde_json::to_value(&unreleased.release).unwrap(),
        serde_json::json!({"released": false})
    );
    rt(unreleased);
    rt(WorkflowVersionDetail {
        id: WorkflowVersionId::new("abc:def"),
        name: "wf".into(),
        project: "proj".into(),
        source: serde_json::json!({"kind": "s3", "uri": "s3://b/k"}),
        git_sha: "abc".into(),
        source_content_hash: "def".into(),
        image_digest: "sha256:x".into(),
        image_repository: Some("repo".into()),
        image_registry_id: Some("123".into()),
        image_region: Some("us-east-1".into()),
        engine: "nextflow".into(),
        engine_version: "24".into(),
        entrypoint: "main.nf".into(),
        target: Some("aws".into()),
        form_schema: serde_json::json!({"fields": [], "warnings": []}),
        queue_classes: vec!["default".into()],
        created_by: Some("user".into()),
        created_by_label: Some("User Name".into()),
        created_at_micros: 42,
        about_markdown: Some("# About\n\nAligns reads.".into()),
        retention_days: None,
        build: 2,
        registration_inputs: RegistrationInputs {
            source_mode: "s3".into(),
            cli_version: "0.0.0".into(),
            overrides: RegistrationOverrides::default(),
            reregistered_from: None,
        },
        runtime: Some(VersionRuntime {
            key: "k".into(),
            recipe: serde_json::json!({"engine": "snakemake", "engine_version": "9.27.0"}),
            state: "ready".into(),
            ready: true,
            build_arn: Some("arn:aws:codebuild:us-east-1:123:build/p:1".into()),
            reason: None,
            logs_url: None,
            builds: 1,
            coordinator_image: Some("repo@sha256:v".into()),
            job_definition: Some("arn:aws:batch:us-east-1:123:job-definition/x:1".into()),
        }),
        images: vec![WorkflowImage {
            name: "fastqc".into(),
            declared_ref: "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0".into(),
            source_ref: "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0@sha256:e1".into(),
            state: "ready".into(),
            image: Some("123.dkr.ecr.us-east-1.amazonaws.com/wf@sha256:e1".into()),
            archs: vec!["amd64".into()],
            capabilities: serde_json::json!({"bash": true, "libc": "glibc"}),
            archived: true,
            keep_tag: Some("biffix-keep-e1".into()),
            derived_image: Some("123.dkr.ecr.us-east-1.amazonaws.com/wf@sha256:d1".into()),
            derived_state: Some("ready".into()),
            reason: None,
            logs_url: None,
            kind: "import".into(),
            generated_dockerfile: None,
            arch_builds: vec![],
        }],
        state: Some("ready".into()),
        not_ready: vec![],
        release: VersionRelease::default(),
        can_release: true,
    });
}

#[test]
fn submit_round_trips() {
    rt(RunSubmitRequest {
        experiment: None,
        retry_of: None,
        workflow_version_id: "abc:def".into(),
        params: serde_json::json!({"reads": "s3://b/r.fastq"}),
        target: None,
        output_prefix: None,
        retries: None,
        keep_going: None,
        resume_from: None,
        run_name: None,
        tables: None,
        reference: None,
        retention: None,
        images: None,
    });
    // A submit carrying target, output-prefix, retries, keep-going and resume overrides must
    // round-trip.
    rt(RunSubmitRequest {
        experiment: Some("exp-1".into()),
        retry_of: None,
        workflow_version_id: "abc:def".into(),
        params: serde_json::json!({"reads": "s3://b/r.fastq"}),
        target: Some("smoke".into()),
        output_prefix: Some("s3://b/".into()),
        retries: Some(5),
        keep_going: Some(false),
        resume_from: Some(RunId::new("run-0")),
        run_name: Some("batch-12".into()),
        tables: None,
        reference: None,
        retention: None,
        images: Some(vec!["repo/tool:1.0".into()]),
    });
    rt(SubmittedRun {
        run_id: RunId::new("run-1"),
        state: "submitted".into(),
        coordinator_job_id: Some("job-1".into()),
        assumed_role_arn: Some("arn:aws:iam::1:role/r".into()),
    });
    // A run still preparing its images has no coordinator yet.
    rt(SubmittedRun {
        run_id: RunId::new("run-2"),
        state: "preparing_images".into(),
        coordinator_job_id: None,
        assumed_role_arn: None,
    });
}

#[test]
fn run_list_and_detail_round_trip_minimal() {
    // Minimal summary: every optional dropped on the wire, restored by default on the way back.
    let minimal = RunSummary {
        experiment_id: None,
        cost_usd: None,
        cost_pending: false,
        run_id: RunId::new("run-1"),
        state: "submitted".into(),
        terminal: false,
        completed: None,
        total: None,
        submitted_at: None,
        completed_at: None,
        submitter: None,
        submitter_label: None,
        run_name: None,
        workflow: None,
        workflow_name: None,
        workflow_title: None,
        engine: None,
        project: None,
        tasks_observed: 0,
        tasks_succeeded: 0,
        tasks_running: 0,
        tasks_queued: 0,
        tasks_failed: 0,
        tasks_cached: 0,
        outputs_total: 0,
        outputs_reused: 0,
        resumed: false,
        input_uris: vec![],
        samples: None,
    };
    rt(minimal.clone());
    rt(RunPage {
        runs: vec![minimal.clone()],
        total: 1,
        limit: 50,
        first_cursor: None,
        last_cursor: None,
    });
    rt(RunDetail {
        cancelled_by: None,
        summary: minimal,
        tasks: vec![Task {
            task_key: TaskKey::new("t1"),
            state: "pending".into(),
            attempts: 0,
            exit_code: None,
            batch_job_id: None,
            log_stream: None,
            termination_category: None,
            instance_id: None,
            started_at: None,
            finished_at: None,
            cached: false,
            group: None,
            deps: vec!["t0".into()],
            feeds: vec![],
            cost_usd: Some(0.25),
            host: None,
            spot_warned_at: None,
            image: Some("repo/x@sha256:1".into()),
            verdict_reason: Some("missing outputs".into()),
            requested_vcpus: Some(4.0),
            requested_memory_mib: Some(30_000),
        }],
        params: serde_json::json!({}),
        output_prefix: None,
        coordinator_job_id: None,
        coordinator_log_stream: None,
        coordinator_host: None,
        engine_session_id: Some("3f6c2a1e-0000-4000-8000-000000000000".into()),
        session_mismatch: true,
        resumed_from: Some(RunId::new("run-0")),
        workflow_registered_at_micros: Some(1_700_000_000_000_000),
        launched_at_micros: Some(1_700_000_100_000_000),
        input_tables: None,
        cost: Some(RunCost {
            estimated_usd: 1.5,
            compute_usd: 1.25,
            ebs_usd: 0.25,
            coordinator_usd: 0.5,
            winning_usd: 1.0,
            priced_jobs: 3,
            pending_jobs: 1,
            price_sources: vec!["on_demand".into(), "spot_history".into()],
            by_instance_type: vec![CostByInstanceType {
                instance_type: "c6a.4xlarge".into(),
                lifecycle: Some("spot".into()),
                jobs: 2,
                hours: 1.5,
                usd: 1.0,
            }],
        }),
        provenance: Some(biffix_api::RunProvenance {
            git_sha: "abc".into(),
            image_digest: "sha256:derived".into(),
            image_source_digest: Some("sha256:imported".into()),
            runtime: Some(biffix_api::RuntimeUse {
                engine_version: "9.27.0".into(),
                runtime_key: "k".into(),
                coordinator_image: "repo@sha256:c".into(),
            }),
            image_map: Some([("a".to_string(), "b".to_string())].into_iter().collect()),
            submit_images: Some(vec![biffix_api::SubmittedImage {
                declared_ref: "repo/tool:1.0".into(),
                source_ref: "repo/tool:1.0@sha256:a".into(),
            }]),
        }),
        submit_images: Some(vec![biffix_api::SubmittedImage {
            declared_ref: "repo/tool:1.0".into(),
            source_ref: "repo/tool:1.0@sha256:a".into(),
        }]),
        preparing: Some(RunPreparation {
            images: vec![serde_json::from_value(serde_json::json!({
                "name": "repo/tool:1.0",
                "declared_ref": "repo/tool:1.0",
                "source_ref": "repo/tool:1.0@sha256:a",
                "state": "building",
            }))
            .unwrap()],
            not_ready: vec!["image `repo/tool:1.0` is being imported".into()],
            since_micros: 1_000_000,
        }),
        dag: RunDag { nodes: vec![DagNode::default()], edges: vec![] },
        reference_bundle: None,
        retention: None,
        work_deleted_at_micros: None,
        resumable: true,
        resume_cache_uri: Some("s3://b/p/.nextflow/cache/s/".into()),
        prefix_kind: Some(biffix_api::OutputPrefixKind::Resumed),
    });
}

#[test]
fn run_detail_flatten_puts_summary_fields_at_top_level() {
    // The summary is `#[serde(flatten)]`, so the wire form is flat: `run_id` sits beside `tasks`,
    // not under a `summary` key. This is what keeps the flattened wire shape stable.
    let detail = RunDetail {
        cancelled_by: None,
        summary: RunSummary {
            experiment_id: None,
            cost_usd: None,
            cost_pending: false,
            run_id: RunId::new("run-9"),
            state: "succeeded".into(),
            terminal: true,
            completed: Some(3),
            total: Some(3),
            submitted_at: None,
            completed_at: None,
            submitter: None,
            submitter_label: None,
            run_name: None,
            workflow: Some(WorkflowVersionId::new("abc:def")),
            workflow_name: None,
            workflow_title: None,
            engine: None,
            project: None,
            tasks_observed: 3,
            tasks_succeeded: 3,
            tasks_running: 0,
            tasks_queued: 0,
            tasks_failed: 0,
            tasks_cached: 1,
            outputs_total: 0,
            outputs_reused: 0,
            resumed: false,
            input_uris: vec![],
            samples: None,
        },
        tasks: vec![],
        params: serde_json::json!({}),
        output_prefix: None,
        coordinator_job_id: Some(CoordinatorJobId::new("job-9")),
        coordinator_log_stream: None,
        coordinator_host: None,
        engine_session_id: None,
        session_mismatch: false,
        resumed_from: None,
        workflow_registered_at_micros: None,
        launched_at_micros: None,
        input_tables: None,
        cost: None,
        provenance: None,
        submit_images: None,
        preparing: None,
        dag: RunDag { nodes: vec![], edges: vec![] },
        reference_bundle: None,
        retention: None,
        work_deleted_at_micros: None,
        resumable: false,
        resume_cache_uri: None,
        prefix_kind: None,
    };
    let v = serde_json::to_value(&detail).unwrap();
    assert_eq!(v["run_id"], "run-9", "summary must be flattened to top level");
    assert_eq!(v["state"], "succeeded");
    assert!(v.get("summary").is_none(), "no nested `summary` key on the wire");
    assert_eq!(v["terminal"], true);
    assert_eq!(v["resumable"], false);
    for absent in
        ["engine_session_id", "session_mismatch", "resumed_from", "resume_cache_uri", "prefix_kind"]
    {
        assert!(v.get(absent).is_none(), "{absent} is omitted when unset");
    }
    rt(detail);
}

proptest! {
    /// Fuzz the run summary: finite floats only (NaN/Inf don't survive JSON), so `PartialEq` holds.
    #[test]
    fn run_summary_round_trips(
        run_id in "[a-z0-9-]{1,16}",
        state in "[a-z]{3,10}",
        completed in proptest::option::of(0i64..1000),
        total in proptest::option::of(0i64..1000),
        submitter in proptest::option::of("[a-z]{1,8}"),
        // Integer-valued epoch seconds cast to f64: exercises the field while round-tripping
        // exactly (serde_json's default float parser is not bit-exact for arbitrary fractions,
        // which is irrelevant to a timestamp and out of scope for this contract test).
        submitted_at in proptest::option::of((0i64..2_000_000_000).prop_map(|s| s as f64)),
        input_uris in proptest::collection::vec("s3://[a-z/]{3,20}", 0..4),
        tasks_succeeded in 0i64..1000,
        experiment_id in proptest::option::of("exp-[a-z0-9-]{1,12}"),
        experiment in proptest::option::of("exp-[a-z0-9-]{1,12}"),
        // Whole dollars, for the same reason as `submitted_at`.
        cost_usd in proptest::option::of((0i64..100_000).prop_map(|d| d as f64)),
        cost_pending in proptest::bool::ANY,
        resumed in proptest::bool::ANY,
    ) {
        rt(RunSubmitRequest {
            experiment,
            retry_of: None,
            workflow_version_id: "abc:def".into(),
            params: serde_json::json!({}),
            target: None,
            output_prefix: None,
            retries: None,
            keep_going: None,
            resume_from: None,
            run_name: None,
            tables: None,
            reference: None,
            retention: None,
            images: None,
        });
        rt(RunSummary {
            experiment_id,
            cost_usd,
            cost_pending,
            run_id: RunId::new(run_id),
            state,
            terminal: false,
            completed,
            total,
            submitted_at,
            completed_at: None,
            submitter,
            submitter_label: None,
            run_name: None,
            workflow: None,
            workflow_name: None,
            workflow_title: Some("Bulk RNA-seq".into()),
            engine: None,
            project: None,
            tasks_observed: tasks_succeeded,
            tasks_succeeded,
            tasks_running: 0,
            tasks_queued: 0,
            tasks_failed: 0,
            tasks_cached: 0,
            outputs_total: 0,
            outputs_reused: 0,
            resumed,
            input_uris,
            samples: None,
        });
    }
}

#[test]
fn run_samples_is_the_core_s_sample_counts_on_the_wire() {
    let counts = biffix_core::sample_progress::SampleCounts {
        total: 12,
        done: 9,
        failed: 1,
        declared: true,
    };
    let wire = RunSampleCounts::from(counts);
    assert_eq!((wire.total, wire.done, wire.failed, wire.declared), (12, 9, 1, true));
    rt(wire);
}

#[test]
fn an_experiment_update_tells_absent_from_null_from_a_value() {
    let cases = [
        (serde_json::json!({}), None),
        (serde_json::json!({"description": null}), Some(None)),
        (serde_json::json!({"description": "x"}), Some(Some("x".to_string()))),
    ];
    for (wire, want) in cases {
        let update: ExperimentUpdateRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(update.description, want, "{wire}");
        rt(update);
    }
}

#[test]
fn a_run_experiment_set_must_name_experiment_id_even_as_null() {
    for (wire, want) in [
        (serde_json::json!({"experiment_id": "exp-1"}), Some("exp-1")),
        (serde_json::json!({"experiment_id": null}), None),
    ] {
        let set: RunExperimentSetRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(set.experiment_id.as_deref(), want, "{wire}");
        rt(set);
    }
    for wire in [serde_json::json!({}), serde_json::json!({"experimentId": "exp-1"})] {
        assert!(serde_json::from_value::<RunExperimentSetRequest>(wire.clone()).is_err(), "{wire}");
    }
}

#[test]
fn a_run_rename_must_name_run_name_even_as_null() {
    for (wire, want) in [
        (serde_json::json!({"run_name": "Liver batch 2"}), Some("Liver batch 2")),
        (serde_json::json!({"run_name": null}), None),
    ] {
        let rename: RunRenameRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(rename.run_name.as_deref(), want, "{wire}");
        rt(rename);
    }
    for wire in [serde_json::json!({}), serde_json::json!({"name": "x"})] {
        assert!(serde_json::from_value::<RunRenameRequest>(wire.clone()).is_err(), "{wire}");
    }
    // A clear is sent with the field, as `null`: never left out (which the server refuses).
    assert_eq!(
        serde_json::to_value(RunRenameRequest { run_name: None }).unwrap(),
        serde_json::json!({"run_name": null})
    );
}

#[test]
fn a_notification_round_trips_with_its_title_and_start() {
    rt(Notification {
        id: "r1:failed".into(),
        run_id: "r1".into(),
        run_name: None,
        workflow_name: Some("nf-core/rnaseq".into()),
        workflow_title: Some("Bulk RNA-seq".into()),
        submitted_at: Some(1.5),
        kind: "failed".into(),
        at: 2.5,
        read: false,
        samples_total: Some(3),
        samples_failed: Some(1),
    });
}

#[test]
fn qc_previews_round_trip() {
    rt(FastqcReport {
        output_id: "o1".into(),
        html_output_id: None,
        version: None,
        modules: vec![FastqcModule {
            name: "Basic Statistics".into(),
            status: "pass".into(),
            columns: vec![],
            rows: vec![],
            extra: vec![],
            truncated: false,
        }],
    });
    rt(FastqcReport {
        output_id: "o1".into(),
        html_output_id: Some("o2".into()),
        version: Some("0.12.1".into()),
        modules: vec![FastqcModule {
            name: "Sequence Duplication Levels".into(),
            status: "warn".into(),
            columns: vec!["Duplication Level".into(), "Percentage".into()],
            rows: vec![vec!["1".into(), "90.5".into()]],
            extra: vec![["Total Deduplicated Percentage".into(), "94.08".into()]],
            truncated: true,
        }],
    });
    let alignment = AlignmentTrack {
        name: "S1.bam".into(),
        format: "bam".into(),
        url: "https://a".into(),
        index_url: "https://a.bai".into(),
    };
    rt(AlignmentLinks {
        alignment: alignment.clone(),
        reference: None,
        locus: None,
        expires_at: 1.5,
        bucket_cors: "unknown".into(),
        notes: vec![],
    });
    for reference in [
        AlignmentReference::Bundle {
            bundle_id: "b".into(),
            display_name: "B".into(),
            fasta_url: "https://f".into(),
            fai_url: "https://f.fai".into(),
            gzi_url: None,
            annotation: None,
        },
        AlignmentReference::Bundle {
            bundle_id: "b".into(),
            display_name: "B".into(),
            fasta_url: "https://f".into(),
            fai_url: "https://f.fai".into(),
            gzi_url: Some("https://f.gzi".into()),
            annotation: Some(AnnotationTrack {
                url: "https://g".into(),
                index_url: Some("https://g.tbi".into()),
                format: "gtf".into(),
            }),
        },
        AlignmentReference::IgvGenome {
            genome: "hg38".into(),
            bundle_id: None,
            display_name: None,
        },
    ] {
        rt(AlignmentLinks {
            alignment: alignment.clone(),
            reference: Some(reference),
            locus: Some("chrB:1-2000".into()),
            expires_at: 1.5,
            bucket_cors: "configured".into(),
            notes: vec!["n".into()],
        });
    }
    // The reference names its source.
    let v = serde_json::to_value(AlignmentReference::IgvGenome {
        genome: "mm39".into(),
        bundle_id: Some("b".into()),
        display_name: None,
    })
    .unwrap();
    assert_eq!(v, serde_json::json!({"source": "igv_genome", "genome": "mm39", "bundle_id": "b"}));
}

#[test]
fn a_run_report_s_declaration_is_optional_on_the_wire() {
    let wire = serde_json::json!({
        "name": "QC", "kind": "declared", "format": "html", "sample": "S1", "declaration": 2,
        "output": {
            "id": "o1", "s3_uri": "s3://b/r/qc/S1.html", "path": "qc/S1.html", "name": "S1.html",
            "role": "output", "servable": true
        }
    });
    let report: RunReport = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(report.declaration, Some(2));
    rt(report.clone());
    assert_eq!(serde_json::to_value(&report).unwrap()["declaration"], 2);
    // A report no declaration produced (a detected one) has none, and none is written.
    let mut detected = wire;
    detected.as_object_mut().unwrap().remove("declaration");
    let report: RunReport = serde_json::from_value(detected).unwrap();
    assert_eq!(report.declaration, None);
    assert!(serde_json::to_value(&report).unwrap().get("declaration").is_none());
}

#[test]
fn project_routing_fields_round_trip() {
    rt(SourceUploadRequest {
        version_id: "abc:hash".into(),
        context_tree_hash: None,
        project: "imported".into(),
    });
    for project in [None, Some("imported".to_string())] {
        rt(DataListRequest {
            source: "project".into(),
            path: None,
            starts_with: None,
            cursor: None,
            project: project.clone(),
        });
        rt(UploadKeyRequest { key: "uploads/a.fq.gz".into(), project });
    }
    rt(Project {
        id: "genomics".into(),
        bucket: "acme-genomics".into(),
        uploads: true,
        viewer_cors: false,
        default: true,
    });
}

#[test]
fn attempt_view_round_trips_its_requested_resources() {
    let attempt = TaskAttempt {
        batch_job_id: "job-1".into(),
        attempt: Some(1),
        state: "running".into(),
        exit_code: None,
        termination_category: None,
        status_reason: None,
        log_stream: None,
        queue: Some("q".into()),
        instance_id: None,
        started_at: None,
        finished_at: None,
        display: true,
        host: None,
        spot_warned_at: None,
        cost: None,
        requested_vcpus: Some(0.25),
        requested_memory_mib: Some(512),
    };
    rt(attempt.clone());
    let json = serde_json::to_value(&attempt).unwrap();
    assert_eq!(json["requested_vcpus"], serde_json::json!(0.25));
    assert_eq!(json["requested_memory_mib"], serde_json::json!(512));
    // Unknown: omitted on the wire, and a body without the fields reads back.
    let unknown = TaskAttempt { requested_vcpus: None, requested_memory_mib: None, ..attempt };
    let json = serde_json::to_value(&unknown).unwrap();
    assert!(json.get("requested_vcpus").is_none());
    assert!(json.get("requested_memory_mib").is_none());
    rt(unknown);
}

/// The minimal form of each response the CLI or the UI reads that no case above covers.
#[test]
fn minimal_responses_round_trip() {
    let output = RunOutput {
        id: "o1".into(),
        s3_uri: "s3://b/o".into(),
        path: "o".into(),
        name: "o".into(),
        task_key: None,
        attempt: None,
        role: "output".into(),
        reused: false,
        reused_from: None,
        size: None,
        etag: None,
        last_modified: None,
        exists: None,
        modified_since_produced: false,
        servable: false,
        viewer: None,
    };
    rt(output.clone());
    rt(RunOutputPage {
        outputs: vec![output],
        total: 1,
        output_prefix: None,
        next_cursor: None,
        enrichment_pending: 0,
    });
    rt(Host::default());
    rt(FailureSummary {
        failed_steps: 1,
        items: vec![FailureItem {
            task_key: None,
            step: "align".into(),
            cause: "unknown".into(),
            sample: None,
            try_again: "resume".into(),
            message: "it failed".into(),
            advice: "look".into(),
            attempts: 1,
            log_tail: vec![],
            reason: None,
            stage: None,
            exit_code: None,
            batch_job_id: None,
        }],
        support: None,
    });
    let experiment = Experiment {
        id: "e1".into(),
        project: "p".into(),
        name: "e".into(),
        description: None,
        created_by: None,
        created_by_label: None,
        created_at: 1.0,
        archived_at: None,
        run_count: 0,
        last_run_at: None,
        needs_attention: 0,
        mine: false,
    };
    rt(experiment.clone());
    rt(ExperimentDetail {
        experiment,
        runs: vec![],
        runs_total: 0,
        samples: vec![],
        total_cost_usd: None,
    });
    rt(UserPrefs::default());
    let file = DataFile {
        name: "a".into(),
        path: "a".into(),
        uri: "s3://b/a".into(),
        size: 0,
        last_modified_micros: None,
        etag: None,
        viewer: None,
    };
    rt(file.clone());
    rt(DataFolderPage {
        source: "s".into(),
        path: String::new(),
        folders: vec![],
        files: vec![file],
        next_cursor: None,
    });
    rt(LogPage {
        log_stream: None,
        events: vec![],
        next_forward_token: None,
        next_backward_token: None,
    });
    rt(TaskPage {
        tasks: vec![],
        total: 0,
        offset: 0,
        limit: 50,
        counts: TaskFilterCounts::default(),
        log_group: None,
    });
    rt(ReferenceBundle {
        id: "r".into(),
        kind: "genome".into(),
        display_name: "R".into(),
        uris: Default::default(),
        notes: None,
        igv_genome: None,
        created_at_micros: 0,
        updated_at_micros: 0,
        created_by: None,
    });
}
