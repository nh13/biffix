//! The run provenance crate (WRROC 0.6 Provenance Run Crate): its shape, what it records, the
//! `GET /runs/{id}/provenance` route and the sweep that writes each ended run's copy to S3.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use biffix_api::{Host, Task};
use biffix_backend::api;
use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
use biffix_backend::runs::provenance::{
    build_crate, crate_uri, write_due, AnchorFacts, ProvenanceInput,
};
use biffix_backend::store::outputs::OutputRow;
use biffix_backend::store::registry::RegisteredVersion;
use biffix_backend::store::RunDetailView;
use biffix_backend::testing::{self, get_json, FakeCloud, MemRegistry, MemStore, TestState};
use biffix_backend::AppState;
use biffix_core::ids::RunId;
use biffix_workflow::engines::Engine;

fn task(key: &str, group: &str, state: &str, start: f64, end: f64) -> Task {
    Task {
        exit_code: Some(if state == "failed" { 137 } else { 0 }),
        termination_category: (state == "failed").then(|| "out_of_memory".to_string()),
        started_at: Some(start),
        finished_at: Some(end),
        group: Some(group.into()),
        cost_usd: Some(0.0125),
        host: Some(Host { instance_type: Some("c6a.large".into()), ..Default::default() }),
        ..testing::task_view(key, state, None, Some(&format!("job-{key}")))
    }
}

fn output(uri: &str, task: &str, role: &str) -> OutputRow {
    OutputRow {
        id: uri.into(),
        size: Some(42),
        etag: Some("\"etag-1\"".into()),
        ..testing::output_row("run-1", uri, Some(task), role)
    }
}

fn version() -> RegisteredVersion {
    RegisteredVersion {
        name: "align".into(),
        project: "bench".into(),
        source: WorkflowSource::Git {
            repo: "https://github.com/example/align.git".into(),
            git_ref: "main".into(),
        },
        version: WorkflowVersion {
            id: "abc123:hash".into(),
            git_sha: "abc123".into(),
            source_content_hash: "hash".into(),
            image_digest: "sha256:feedbeef".into(),
            image_repository: Some("biffix-workflows".into()),
            image_registry_id: Some("123456789012".into()),
            image_region: Some("us-east-1".into()),
            engine: Engine::Snakemake,
            engine_version: "9.27".into(),
            entrypoint: "workflow/Snakefile".into(),
            target: None,
            form_schema: json!({}),
            queue_classes: vec![],
            about_markdown: None,
            runtime_key: "rt".into(),
            runtime_recipe: json!({"engine": "snakemake"}),
            build: 0,
        },
        created_by: None,
        created_by_label: None,
        created_at_micros: 0,
        registration_inputs: Default::default(),
        registration_inputs_hash: String::new(),
        images: Vec::new(),
    }
}

/// A finished two-step run: `align` feeds `stats`; the stats task failed.
fn detail() -> RunDetailView {
    let mut d = testing::run_detail("run-1", "failed");
    d.run.workflow_name = Some("align".into());
    d.run.engine = Some("snakemake".into());
    d.run.submitted_at = Some(1_790_000_000.0);
    d.run.completed_at = Some(1_790_000_600.0);
    d.run.submitter = Some("user:abc".into());
    d.run.submitter_label = Some("someone@example.com".into());
    d.run.input_uris = vec!["s3://bkt/in/reads.fq.gz".into()];
    d.params = json!({
        "reads": "s3://bkt/in/reads.fq.gz",
        "outdir": "s3://bkt/results/",
        "threads": 4,
        "api_key": "***",
    });
    // Listed consumer-first, so the order must come from the edges, not the list.
    d.dag_nodes = vec!["stats".into(), "align".into()];
    d.dag_edges = vec![("align".into(), "stats".into())];
    d.tasks = vec![
        task("align:sample=A", "align", "succeeded", 1_790_000_010.0, 1_790_000_300.0),
        task("stats:sample=A", "stats", "failed", 1_790_000_310.0, 1_790_000_500.0),
    ];
    d.reference_bundle = Some(json!({"id": "grch38", "uris": {"fasta": "s3://refs/hg38.fa"}}));
    testing::launched_at(d, Some(1_790_000_005_000_000))
}

fn outputs() -> Vec<OutputRow> {
    vec![
        output("s3://bkt/runs/run-1/A.bam", "align:sample=A", "published"),
        output("s3://bkt/runs/run-1/work/tmp.sam", "align:sample=A", "intermediate"),
    ]
}

fn anchor() -> AnchorFacts {
    AnchorFacts {
        git_sha: "abc123".into(),
        image_digest: "sha256:feedbeef".into(),
        input_provenance: BTreeMap::from([(
            "s3://bkt/in/reads.fq.gz".to_string(),
            "v-111".to_string(),
        )]),
        ..Default::default()
    }
}

fn build(
    d: &RunDetailView,
    v: Option<&RegisteredVersion>,
    a: Option<&AnchorFacts>,
    o: &[OutputRow],
) -> Value {
    build_crate(&ProvenanceInput {
        detail: d,
        version: v,
        anchor: a,
        outputs: o,
        outputs_total: None,
        events_uri: Some("s3://bkt/biffix/runs/run-1/events.jsonl"),
        generated_at_micros: 1_790_000_700_000_000,
        biffix_version: "0.1.0",
        experiment: None,
    })
}

fn crate_json() -> Value {
    let (d, v, a, o) = (detail(), version(), anchor(), outputs());
    build(&d, Some(&v), Some(&a), &o)
}

fn entity<'a>(c: &'a Value, id: &str) -> &'a Value {
    find(c, id).unwrap_or_else(|| panic!("no entity {id}"))
}

fn find<'a>(c: &'a Value, id: &str) -> Option<&'a Value> {
    c["@graph"].as_array().unwrap().iter().find(|e| e["@id"] == id)
}

fn ids(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().flat_map(ids).collect(),
        Value::Object(o) => {
            o.get("@id").and_then(Value::as_str).map(|s| vec![s.to_string()]).unwrap_or_default()
        }
        _ => vec![],
    }
}

/// Every `@id` in the graph, and whether any appears twice.
fn graph_ids(c: &Value) -> (BTreeSet<String>, bool) {
    let mut seen = BTreeSet::new();
    let mut dup = false;
    for e in c["@graph"].as_array().unwrap() {
        dup |= !seen.insert(e["@id"].as_str().unwrap().to_string());
    }
    (seen, dup)
}

#[test]
fn declares_the_wrroc_profiles_and_a_workflow_main_entity() {
    let c = crate_json();
    assert_eq!(c["@context"][0], "https://w3id.org/ro/crate/1.3/context");
    assert_eq!(entity(&c, "ro-crate-metadata.json")["about"]["@id"], "./");
    let root = entity(&c, "./");
    let profiles = ids(&root["conformsTo"]);
    for p in [
        "https://w3id.org/ro/wfrun/process/0.6",
        "https://w3id.org/ro/wfrun/workflow/0.6",
        "https://w3id.org/ro/wfrun/provenance/0.6",
        "https://w3id.org/workflowhub/workflow-ro-crate/1.1",
    ] {
        assert!(profiles.contains(&p.to_string()), "missing {p}");
    }
    let wf_id = root["mainEntity"]["@id"].as_str().unwrap();
    assert_eq!(wf_id, "https://github.com/example/align/blob/abc123/workflow/Snakefile");
    let wf = entity(&c, wf_id);
    for t in ["File", "SoftwareSourceCode", "ComputationalWorkflow", "HowTo"] {
        assert!(wf["@type"].as_array().unwrap().contains(&json!(t)));
    }
    assert_eq!(
        wf["programmingLanguage"]["@id"],
        "https://w3id.org/workflowhub/workflow-ro-crate#snakemake"
    );
}

#[test]
fn every_reference_resolves_to_an_entity_or_a_profile() {
    let mut a = anchor();
    a.image_digest = "sha256:derived".into();
    a.image_source_digest = Some("sha256:feedbeef".into());
    a.image_map = BTreeMap::from([(
        "quay.io/b/fastqc:1".to_string(),
        "123.dkr.ecr.us-east-1.amazonaws.com/w@sha256:fastqc".to_string(),
    )]);
    let (d, v, o) = (detail(), version(), outputs());
    for c in [crate_json(), build(&d, Some(&v), Some(&a), &o)] {
        let (defined, dup) = graph_ids(&c);
        assert!(!dup, "an @id appears twice");
        // The metadata descriptor's `conformsTo` names the RO-Crate spec by permalink, and a
        // language's `identifier`/`url` point at its website: external links, not entities.
        for e in c["@graph"].as_array().unwrap() {
            if e["@id"] == "ro-crate-metadata.json" {
                continue;
            }
            for (key, value) in e.as_object().unwrap() {
                if matches!(key.as_str(), "@id" | "@type" | "additionalType" | "identifier" | "url")
                {
                    continue;
                }
                for id in ids(value) {
                    assert!(defined.contains(&id), "{key} -> {id} is not in the graph");
                }
            }
        }
    }
}

#[test]
fn steps_are_ordered_producer_first_and_tasks_link_step_to_tool() {
    let c = crate_json();
    assert_eq!(entity(&c, "#step/align")["position"], "0");
    assert_eq!(entity(&c, "#step/stats")["position"], "1");

    let task = entity(&c, "#task/align%3Asample%3DA");
    assert_eq!(task["@type"], "CreateAction");
    assert_eq!(task["instrument"]["@id"], "#tool/align");
    assert_eq!(task["actionStatus"], "http://schema.org/CompletedActionStatus");
    assert_eq!(task["startTime"], "2026-09-21T14:13:30+00:00");
    assert_eq!(task["containerImage"]["@id"], "#image");
    // Its outputs, intermediates included, are its results, and nothing else's.
    let mut results = ids(&task["result"]);
    results.sort();
    assert_eq!(
        results,
        ["s3://bkt/runs/run-1/A.bam".to_string(), "s3://bkt/runs/run-1/work/tmp.sam".to_string()]
    );
    assert_eq!(
        ids(&task["resourceUsage"]),
        vec![
            "#usage/align%3Asample%3DA/attempts",
            "#usage/align%3Asample%3DA/instanceType",
            "#usage/align%3Asample%3DA/estimatedCost",
        ]
    );

    let control = entity(&c, "#step-run/align%3Asample%3DA");
    assert_eq!(control["@type"], "ControlAction");
    assert_eq!(control["instrument"]["@id"], "#step/align");
    assert_eq!(control["object"]["@id"], "#task/align%3Asample%3DA");
    let engine = entity(&c, "#engine-run");
    assert_eq!(engine["@type"], "OrganizeAction");
    assert_eq!(ids(&engine["object"]).len(), 2);
}

#[test]
fn a_failed_task_says_why() {
    let c = crate_json();
    let task = entity(&c, "#task/stats%3Asample%3DA");
    assert_eq!(task["actionStatus"], "http://schema.org/FailedActionStatus");
    assert_eq!(task["error"], "exit code 137; out of memory");
    assert_eq!(entity(&c, "#run-run-1")["actionStatus"], "http://schema.org/FailedActionStatus");
}

#[test]
fn records_inputs_at_their_submit_version_params_and_published_results() {
    let c = crate_json();
    let run = entity(&c, "#run-run-1");
    assert_eq!(run["instrument"]["@id"], entity(&c, "./")["mainEntity"]["@id"]);
    let object = ids(&run["object"]);
    assert!(object.contains(&"s3://bkt/in/reads.fq.gz".to_string()));
    assert!(object.contains(&"s3://refs/hg38.fa".to_string()));
    assert!(object.contains(&"#param-value/threads".to_string()));
    assert_eq!(entity(&c, "s3://bkt/in/reads.fq.gz")["version"], "v-111");
    assert_eq!(entity(&c, "#param-value/threads")["value"], "4");
    assert_eq!(entity(&c, "#param/threads")["additionalType"], "Integer");
    // Params come in already redacted; the crate carries the placeholder, never the secret.
    assert_eq!(entity(&c, "#param-value/api_key")["value"], "***");
    // Only published/output files are the run's results.
    assert_eq!(ids(&run["result"]), vec!["s3://bkt/runs/run-1/A.bam"]);
    assert_eq!(entity(&c, "s3://bkt/runs/run-1/A.bam")["contentSize"], "42");
    let image = entity(&c, "#image");
    assert_eq!(image["sha256"], "feedbeef");
    assert_eq!(image["registry"], "123456789012.dkr.ecr.us-east-1.amazonaws.com");
    assert_eq!(image["name"], "biffix-workflows");
    assert!(image.get("isBasedOn").is_none());
    assert_eq!(entity(&c, "#agent/user%3Aabc")["name"], "someone@example.com");
}

#[test]
fn a_file_param_is_realized_by_its_input_file() {
    let c = crate_json();
    let formal = entity(&c, "#param/reads");
    assert_eq!(formal["@type"], "FormalParameter");
    assert_eq!(formal["additionalType"], "File");
    assert_eq!(entity(&c, "s3://bkt/in/reads.fq.gz")["exampleOfWork"]["@id"], "#param/reads");
    assert!(find(&c, "#param-value/reads").is_none());
    // An S3 location that is not an input (where results go) is a plain value.
    assert_eq!(entity(&c, "#param/outdir")["additionalType"], "Text");
    assert_eq!(entity(&c, "#param-value/outdir")["value"], "s3://bkt/results/");
    let wf_id = entity(&c, "./")["mainEntity"]["@id"].as_str().unwrap();
    assert_eq!(ids(&entity(&c, wf_id)["input"]).len(), 4);

    // A list of input files is one parameter with many values.
    let mut d = detail();
    d.run.input_uris = vec!["s3://bkt/in/a.fq".into(), "s3://bkt/in/b.fq".into()];
    d.params = json!({"fastqs": ["s3://bkt/in/a.fq", "s3://bkt/in/b.fq"]});
    let c = build(&d, None, None, &[]);
    let formal = entity(&c, "#param/fastqs");
    assert_eq!(formal["additionalType"], "File");
    assert_eq!(formal["multipleValues"], true);
    for uri in ["s3://bkt/in/a.fq", "s3://bkt/in/b.fq"] {
        assert_eq!(entity(&c, uri)["exampleOfWork"]["@id"], "#param/fastqs");
    }
}

#[test]
fn param_ids_have_one_fragment_whatever_the_workflow_id() {
    let mut v = version();
    v.source = WorkflowSource::Git {
        repo: "https://gitlab.com/example/align.git".into(),
        git_ref: "main".into(),
    };
    let (d, a, o) = (detail(), anchor(), outputs());
    for c in [build(&d, Some(&v), Some(&a), &o), build(&d, None, None, &o)] {
        let wf_id = entity(&c, "./")["mainEntity"]["@id"].as_str().unwrap();
        for id in ids(&entity(&c, wf_id)["input"]) {
            assert_eq!(id.matches('#').count(), 1, "{id}");
            assert!(id.starts_with("#param/"), "{id}");
        }
    }
}

#[test]
fn the_image_is_the_one_the_run_ran_not_the_one_it_declared() {
    // A Snakemake default image the runtime was added to: the anchor has the derived digest.
    let mut a = anchor();
    a.image_digest = "sha256:derived".into();
    a.image_source_digest = Some("sha256:feedbeef".into());
    let (d, v, o) = (detail(), version(), outputs());
    let c = build(&d, Some(&v), Some(&a), &o);
    let image = entity(&c, "#image");
    assert_eq!(image["sha256"], "derived");
    // Where the derived image lives is not the version's record, so it is not claimed.
    assert!(image.get("registry").is_none());
    assert_eq!(image["isBasedOn"]["@id"], "#image-source");
    let source = entity(&c, "#image-source");
    assert_eq!(source["sha256"], "feedbeef");
    assert_eq!(source["name"], "biffix-workflows");

    // The run's image map names the worker image, so the derived image's repository is known.
    a.image_map = BTreeMap::from([(
        "$default".to_string(),
        "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workflows@sha256:derived".to_string(),
    )]);
    let c = build(&d, Some(&v), Some(&a), &o);
    let image = entity(&c, "#image");
    assert_eq!(image["sha256"], "derived");
    assert_eq!(image["registry"], "123456789012.dkr.ecr.us-east-1.amazonaws.com");
    assert_eq!(image["name"], "biffix-workflows");

    // A built default image: the version recorded no digest, the anchor did.
    let mut built = version();
    built.version.image_digest = String::new();
    let mut a = anchor();
    a.image_digest = "sha256:built".into();
    let c = build(&d, Some(&built), Some(&a), &o);
    assert_eq!(entity(&c, "#image")["sha256"], "built");
}

#[test]
fn a_china_partition_image_names_its_com_cn_registry() {
    let mut v = version();
    v.version.image_region = Some("cn-north-1".into());
    let (d, a, o) = (detail(), anchor(), outputs());
    let c = build(&d, Some(&v), Some(&a), &o);
    assert_eq!(
        entity(&c, "#image")["registry"],
        "123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn"
    );
}

#[test]
fn a_run_with_several_images_lists_them_all_and_claims_none_per_task() {
    let mut a = anchor();
    a.image_map = BTreeMap::from([
        (
            "$default".to_string(),
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workflows@sha256:feedbeef"
                .to_string(),
        ),
        (
            "quay.io/b/fastqc:1".to_string(),
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workflows@sha256:fastqc"
                .to_string(),
        ),
    ]);
    let (d, v, o) = (detail(), version(), outputs());
    let c = build(&d, Some(&v), Some(&a), &o);
    let run_images = ids(&entity(&c, "#run-run-1")["containerImage"]);
    assert_eq!(run_images.len(), 2);
    assert_eq!(run_images[0], "#image");
    assert_eq!(entity(&c, &run_images[1])["sha256"], "fastqc");
    // Which task ran which image is not recorded, so no task claims one.
    let task = entity(&c, "#task/align%3Asample%3DA");
    assert!(task.get("containerImage").is_none());

    // Once the engine reports each task's image, each task names its own: the default by its
    // digest, the image-map entity by its reference, and an image outside the map (reported, so it
    // ran) as a new entity the run lists too. A task with no report still claims none.
    let fastqc = "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workflows@sha256:fastqc";
    let mut d = detail();
    d.tasks[0].image = Some(
        "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workflows@sha256:feedbeef".into(),
    );
    d.tasks[1].image = Some(fastqc.into());
    let mut extra = d.tasks[1].clone();
    extra.task_key = biffix_core::ids::TaskKey::new("stats:sample=B");
    extra.image = Some("quay.io/b/multiqc:1".into());
    let mut unreported = d.tasks[1].clone();
    unreported.task_key = biffix_core::ids::TaskKey::new("stats:sample=C");
    unreported.image = None;
    d.tasks.push(extra);
    d.tasks.push(unreported);
    let c = build(&d, Some(&v), Some(&a), &o);
    let image_of = |key: &str| {
        entity(&c, &format!("#task/{}", key.replace(':', "%3A").replace('=', "%3D")))
            .get("containerImage")
            .map(|i| i["@id"].as_str().unwrap().to_string())
    };
    assert_eq!(image_of("align:sample=A").as_deref(), Some("#image"));
    let fastqc_id = image_of("stats:sample=A").unwrap();
    assert_eq!(fastqc_id, run_images[1], "the image-map entity");
    let multiqc_id = image_of("stats:sample=B").unwrap();
    assert_eq!(entity(&c, &multiqc_id)["name"], "quay.io/b/multiqc:1");
    assert!(entity(&c, &multiqc_id).get("sha256").is_none());
    assert_eq!(image_of("stats:sample=C"), None);
    let run_images = ids(&entity(&c, "#run-run-1")["containerImage"]);
    assert_eq!(run_images, ["#image", fastqc_id.as_str(), multiqc_id.as_str()]);

    // A map whose only image is the default changes nothing.
    let d = detail();
    a.image_map.remove("quay.io/b/fastqc:1");
    let c = build(&d, Some(&v), Some(&a), &o);
    assert_eq!(entity(&c, "#run-run-1")["containerImage"]["@id"], "#image");
    assert_eq!(entity(&c, "#task/align%3Asample%3DA")["containerImage"]["@id"], "#image");
}

#[test]
fn task_keys_that_differ_only_in_punctuation_get_distinct_ids() {
    let mut d = detail();
    d.tasks =
        vec![task("a:b=1", "a", "succeeded", 1.0, 2.0), task("a:b_1", "a", "succeeded", 1.0, 2.0)];
    let c = build(&d, None, None, &[]);
    let tasks: BTreeSet<&str> = c["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["@type"] == "CreateAction")
        .filter_map(|e| e["@id"].as_str().filter(|id| id.starts_with("#task/")))
        .collect();
    assert_eq!(tasks.len(), 2);
}

#[test]
fn ids_never_collide_across_kinds() {
    // A key that is another key plus a usage name, and a group named like a step execution.
    let mut d = detail();
    d.tasks = vec![
        task("x", "run-x", "succeeded", 1.0, 2.0),
        task("x-attempts", "x", "succeeded", 1.0, 2.0),
        task("x/attempts", "x", "succeeded", 1.0, 2.0),
    ];
    d.dag_nodes = vec![];
    d.dag_edges = vec![];
    let c = build(&d, None, None, &[]);
    let (_, dup) = graph_ids(&c);
    assert!(!dup, "an @id appears twice");
    for key in ["x", "x-attempts", "x%2Fattempts"] {
        assert_eq!(entity(&c, &format!("#task/{key}"))["@type"], "CreateAction");
        assert_eq!(entity(&c, &format!("#step-run/{key}"))["@type"], "ControlAction");
    }
    assert_eq!(entity(&c, "#step/run-x")["@type"], "HowToStep");
}

#[test]
fn a_nextflow_task_without_a_group_belongs_to_its_full_process() {
    let mut d = detail();
    d.run.engine = Some("nextflow".into());
    let mut t = task("NFCORE_RNASEQ:RNASEQ:FASTQC (s1)", "", "succeeded", 1.0, 2.0);
    t.group = None;
    d.tasks = vec![t];
    d.dag_nodes = vec![];
    d.dag_edges = vec![];
    let c = build(&d, None, None, &[]);
    let task = entity(&c, "#task/NFCORE_RNASEQ%3ARNASEQ%3AFASTQC%20%28s1%29");
    assert_eq!(task["instrument"]["@id"], "#tool/NFCORE_RNASEQ%3ARNASEQ%3AFASTQC");
}

#[test]
fn a_nextflow_key_disambiguated_by_its_index_still_belongs_to_its_process() {
    // nf-biffix keys every tagged task `<name> #<index>` (BiffixObserver.taskKey, order-free so a
    // restarted coordinator keys it the same); the process is still everything before the first ` (`.
    let mut d = detail();
    d.run.engine = Some("nextflow".into());
    let mut t = task("RNASEQ:ALIGN (s1) #3", "", "succeeded", 1.0, 2.0);
    t.group = None;
    d.tasks = vec![t];
    d.dag_nodes = vec![];
    d.dag_edges = vec![];
    let c = build(&d, None, None, &[]);
    let task = entity(&c, "#task/RNASEQ%3AALIGN%20%28s1%29%20%233");
    assert_eq!(task["instrument"]["@id"], "#tool/RNASEQ%3AALIGN");
}

#[test]
fn a_capped_output_list_says_so() {
    let (d, v, a, o) = (detail(), version(), anchor(), outputs());
    let c = build_crate(&ProvenanceInput {
        detail: &d,
        version: Some(&v),
        anchor: Some(&a),
        outputs: &o,
        outputs_total: Some(250_000),
        events_uri: None,
        generated_at_micros: 0,
        biffix_version: "0.1.0",
        experiment: None,
    });
    let description = entity(&c, "./")["description"].as_str().unwrap();
    assert!(
        description.contains("The run has 250000 outputs; this crate lists only 2 of them."),
        "{description}"
    );
    assert!(!entity(&crate_json(), "./")["description"].as_str().unwrap().contains("lists only"));
}

#[test]
fn the_s3_copy_lives_under_the_runs_own_prefix() {
    assert_eq!(
        crate_uri("bkt", "run-1").as_deref(),
        Some("s3://bkt/biffix/runs/run-1/provenance/ro-crate-metadata.json")
    );
    assert_eq!(crate_uri("", "run-1"), None);
}

/// A store holding `detail()` with its version registered, its anchor and outputs (an
/// intermediate among them), in a deploy with project bucket `bkt`.
fn wired_state() -> (AppState, Arc<MemStore>, Arc<FakeCloud>) {
    let store = MemStore::new();
    let mut d = detail();
    d.run.workflow = Some("abc123:hash".to_string().into());
    store.insert(d);
    let mut anchor = biffix_core::event::RunSubmitted::new(
        "abc123:hash",
        "abc123",
        "sha256:derived",
        json!({}),
        vec!["s3://bkt/in/reads.fq.gz".into()],
    );
    anchor.input_provenance =
        Some(BTreeMap::from([("s3://bkt/in/reads.fq.gz".to_string(), "v-111".to_string())]));
    store.insert_anchor(biffix_core::event::run_submitted(
        &RunId::new("run-1"),
        biffix_core::time::Timestamp::from_micros(1_790_000_000_000_000),
        anchor,
    ));
    store.insert_outputs("run-1", outputs());
    let registry = MemRegistry::new();
    registry.versions.lock().unwrap().insert("abc123:hash".into(), version());
    let store = Arc::new(store);
    let cloud = Arc::new(FakeCloud::new());
    let state = TestState::new()
        .with_store(store.clone())
        .with_registry(Arc::new(registry))
        .with_cloud(cloud.clone())
        .with_projects(testing::bucket_project("bkt"))
        .build();
    (state, store, cloud)
}

#[tokio::test]
async fn the_route_serves_the_crate_as_a_download() {
    let store = MemStore::new();
    store.insert(detail());
    let app = api::router(state_with(store));
    let res = app
        .clone()
        .oneshot(Request::builder().uri("/runs/run-1/provenance").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"run-1-ro-crate-metadata.json\""
    );
    let (_, c) = testing::response_json(res).await;
    assert_eq!(entity(&c, "#run-run-1")["identifier"], "run-1");

    let (status, _) = get_json(app, "/runs/nope/provenance").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_route_builds_the_crate_from_the_version_anchor_and_every_output() {
    let (state, _, _) = wired_state();
    let (status, c) = get_json(api::router(state), "/runs/run-1/provenance").await;
    assert_eq!(status, StatusCode::OK, "{c}");
    let wf_id = entity(&c, "./")["mainEntity"]["@id"].as_str().unwrap();
    assert_eq!(entity(&c, wf_id)["identifier"], "abc123:hash");
    assert_eq!(entity(&c, "s3://bkt/in/reads.fq.gz")["version"], "v-111");
    assert_eq!(entity(&c, "#image")["sha256"], "derived");
    // Intermediates are listed too, as their task's results.
    let task = entity(&c, "#task/align%3Asample%3DA");
    let mut results = ids(&task["result"]);
    results.sort();
    assert_eq!(
        results,
        ["s3://bkt/runs/run-1/A.bam".to_string(), "s3://bkt/runs/run-1/work/tmp.sam".to_string()]
    );
    assert_eq!(
        entity(&c, "s3://bkt/biffix/runs/run-1/events.jsonl")["encodingFormat"],
        "application/jsonl"
    );
}

#[tokio::test]
async fn a_run_that_never_launched_lists_no_events_file() {
    // The coordinator uploads `events.jsonl` at its exit: a run failed or cancelled before launch
    // has none to point at.
    let (state, store, _) = wired_state();
    let mut d = detail();
    d.run.workflow = Some("abc123:hash".to_string().into());
    store.insert(testing::launched_at(d, None));
    let (status, c) = get_json(api::router(state), "/runs/run-1/provenance").await;
    assert_eq!(status, StatusCode::OK, "{c}");
    let graph = c["@graph"].as_array().unwrap();
    assert!(
        !graph.iter().any(|e| e["@id"].as_str().is_some_and(|id| id.ends_with("events.jsonl"))),
        "{c}"
    );
}

#[tokio::test]
async fn the_sweep_writes_and_marks_due_crates_and_skips_failures() {
    let (state, store, cloud) = wired_state();
    // A second ended run, due after `run-1`, so a failure on `run-1` must not stop the sweep.
    store.insert(testing::run_detail("run-2", "succeeded"));
    let uri = "s3://bkt/biffix/runs/run-1/provenance/ro-crate-metadata.json";
    let uri2 = "s3://bkt/biffix/runs/run-2/provenance/ro-crate-metadata.json";
    // `ghost` is due but has no run to build a crate from.
    *store.provenance_due.lock().unwrap() =
        vec![(RunId::new("ghost"), 3), (RunId::new("run-1"), 7), (RunId::new("run-2"), 9)];
    assert_eq!(write_due(&state).await.unwrap(), 2);
    assert_eq!(
        *store.provenance_marked.lock().unwrap(),
        vec![(RunId::new("run-1"), 7, uri.to_string()), (RunId::new("run-2"), 9, uri2.to_string()),]
    );
    let written: Value = serde_json::from_slice(&cloud.objects.lock().unwrap()[uri]).unwrap();
    assert_eq!(entity(&written, "#run-run-1")["identifier"], "run-1");
    let written: Value = serde_json::from_slice(&cloud.objects.lock().unwrap()[uri2]).unwrap();
    assert_eq!(entity(&written, "#run-run-2")["identifier"], "run-2");

    // A write that fails is not marked, so the run is due again; the sweep goes on to the next.
    store.provenance_marked.lock().unwrap().clear();
    cloud.objects.lock().unwrap().clear();
    cloud.fail_puts.lock().unwrap().insert(uri.to_string());
    assert_eq!(write_due(&state).await.unwrap(), 1);
    assert_eq!(
        *store.provenance_marked.lock().unwrap(),
        vec![(RunId::new("run-2"), 9, uri2.to_string())]
    );
    assert!(cloud.objects.lock().unwrap().contains_key(uri2));

    // A mark that fails skips that run (its crate is written, but it stays due), not the sweep.
    store.provenance_marked.lock().unwrap().clear();
    cloud.objects.lock().unwrap().clear();
    cloud.fail_puts.lock().unwrap().clear();
    store.fail_mark_provenance.lock().unwrap().insert("run-1".into());
    assert_eq!(write_due(&state).await.unwrap(), 1);
    assert_eq!(
        *store.provenance_marked.lock().unwrap(),
        vec![(RunId::new("run-2"), 9, uri2.to_string())]
    );
    let objects = cloud.objects.lock().unwrap();
    assert!(objects.contains_key(uri), "run-1's crate is written before its mark fails");
    assert!(objects.contains_key(uri2));
}

#[tokio::test]
async fn the_sweep_writes_nothing_without_a_project_bucket() {
    let (mut state, store, cloud) = wired_state();
    state.projects = testing::bucket_project("");
    *store.provenance_due.lock().unwrap() = vec![(RunId::new("run-1"), 7)];
    assert_eq!(write_due(&state).await.unwrap(), 0);
    assert!(cloud.objects.lock().unwrap().is_empty());
    assert!(store.provenance_marked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_run_detail_shows_what_the_run_ran_with() {
    let store = MemStore::new();
    store.insert(detail());
    let anchor = biffix_core::event::run_submitted(
        &RunId::new("run-1"),
        biffix_core::time::Timestamp::from_micros(1_790_000_000_000_000),
        biffix_core::event::RunSubmitted {
            runtime: Some(biffix_core::event::RuntimeUse {
                engine_version: "9.27.0".into(),
                runtime_key: "4dbb7d79".into(),
                coordinator_image: "repo@sha256:coord".into(),
            }),
            image_source_digest: Some("sha256:imported".into()),
            image_map: Some(
                [("quay.io/b/seqkit:2".to_string(), "repo@sha256:seqkit".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..biffix_core::event::RunSubmitted::new(
                "abc:def",
                "abc",
                "sha256:derived",
                json!({}),
                vec![],
            )
        },
    );
    store.insert_anchor(anchor);
    let app = api::router(state_with(store));
    let get = |uri: &'static str| {
        let app = app.clone();
        async move {
            let (status, v) = get_json(app, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {v}");
            v
        }
    };
    for uri in ["/runs/run-1", "/runs/run-1?tasks=false"] {
        let run = get(uri).await;
        assert_eq!(
            run["provenance"],
            json!({
                "git_sha": "abc",
                "image_digest": "sha256:derived",
                "image_source_digest": "sha256:imported",
                "runtime": {
                    "engine_version": "9.27.0",
                    "runtime_key": "4dbb7d79",
                    "coordinator_image": "repo@sha256:coord",
                },
                "image_map": {"quay.io/b/seqkit:2": "repo@sha256:seqkit"},
            }),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_run_without_an_anchor_has_no_provenance_block() {
    let store = MemStore::new();
    store.insert(detail());
    let (status, run) = get_json(api::router(state_with(store)), "/runs/run-1").await;
    // The status first: a non-JSON body would have no `provenance` key either.
    assert_eq!(status, StatusCode::OK, "{run}");
    assert!(run.get("provenance").is_none(), "{run}");
}

fn state_with(store: MemStore) -> AppState {
    TestState::new().with_store(Arc::new(store)).build()
}

#[test]
fn a_run_in_an_experiment_is_part_of_it() {
    let (d, v, a, o) = (detail(), version(), anchor(), outputs());
    let experiment = biffix_backend::runs::provenance::ExperimentRef {
        id: "exp-1".into(),
        name: Some("RNA batch 12".into()),
    };
    let c = build_crate(&ProvenanceInput {
        detail: &d,
        version: Some(&v),
        anchor: Some(&a),
        outputs: &o,
        outputs_total: None,
        events_uri: None,
        generated_at_micros: 0,
        biffix_version: "0.1.0",
        experiment: Some(&experiment),
    });
    assert_eq!(entity(&c, "./")["isPartOf"], serde_json::json!({"@id": "#experiment/exp-1"}));
    let e = entity(&c, "#experiment/exp-1");
    assert_eq!(e["identifier"], "exp-1");
    assert_eq!(e["name"], "RNA batch 12");
    // A run in none names none.
    assert!(entity(&crate_json(), "./").get("isPartOf").is_none());
}

/// Through the route: `run_crate` reads the run's `experiment_id`, names the experiment from the
/// store, and keeps only the id when the experiment is not there.
#[tokio::test]
async fn the_route_names_the_run_s_experiment_and_keeps_its_id_when_it_is_gone() {
    use biffix_backend::store::experiments::ExperimentRecord;
    let (state, store, _) = wired_state();
    store.runs.lock().unwrap().get_mut("run-1").unwrap().run.experiment_id = Some("exp-1".into());
    state
        .experiments
        .create(&ExperimentRecord {
            id: "exp-1".into(),
            project: "p".into(),
            name: "RNA batch 12".into(),
            description: None,
            created_by: None,
            created_by_label: None,
            created_at_micros: 1,
            archived_at_micros: None,
        })
        .await
        .unwrap();
    let (status, c) = get_json(api::router(state), "/runs/run-1/provenance").await;
    assert_eq!(status, StatusCode::OK, "{c}");
    assert_eq!(entity(&c, "./")["isPartOf"], json!({"@id": "#experiment/exp-1"}));
    let e = entity(&c, "#experiment/exp-1");
    assert_eq!((&e["identifier"], &e["name"]), (&json!("exp-1"), &json!("RNA batch 12")));

    // An experiment the store does not have (a fresh store here): the id only, no name.
    let (state, store, _) = wired_state();
    store.runs.lock().unwrap().get_mut("run-1").unwrap().run.experiment_id =
        Some("exp-gone".into());
    let (status, c) = get_json(api::router(state), "/runs/run-1/provenance").await;
    assert_eq!(status, StatusCode::OK, "{c}");
    let e = entity(&c, "#experiment/exp-gone");
    assert_eq!(e["identifier"], "exp-gone");
    assert!(e.get("name").is_none(), "{e}");
}

#[tokio::test]
async fn runs_whose_crates_keep_failing_never_starve_a_fresh_run() {
    let (state, store, _) = wired_state();
    // A sweep's worth of due runs that have no run to build a crate from, listed (oldest) first,
    // then one that can be written.
    let mut due: Vec<(RunId, i64)> =
        (0..25).map(|i| (RunId::new(format!("ghost-{i:02}")), 1)).collect();
    due.push((RunId::new("run-1"), 7));
    *store.provenance_due.lock().unwrap() = due;
    // The first sweep takes the oldest, which all fail ...
    assert_eq!(write_due(&state).await.unwrap(), 0);
    // ... so the next takes the run that has not failed first, and writes it.
    assert_eq!(write_due(&state).await.unwrap(), 1);
    assert_eq!(
        *store.provenance_marked.lock().unwrap(),
        vec![(
            RunId::new("run-1"),
            7,
            "s3://bkt/biffix/runs/run-1/provenance/ro-crate-metadata.json".to_string()
        )]
    );
    // Failing twice, a ghost waits before its next try.
    let failures = store.provenance_failures.lock().unwrap();
    let (attempts, retry_after) = failures["ghost-00"];
    assert_eq!(attempts, 2);
    assert!(retry_after > biffix_core::clock::epoch_micros(), "{retry_after}");
}

#[test]
fn samplesheets_shared_files_and_files_of_two_params_are_each_one_entity() {
    let mut d = detail();
    // A samplesheet biffix wrote, as the anchor records it (built from the event's own type, so a
    // renamed field fails here rather than leaving it out of the crate).
    let sheet = "s3://bkt/biffix/runs/run-1/inputs/samples.csv";
    let table =
        biffix_core::event::InputTable { uri: sheet.into(), etag: Some("e1".into()), rows: vec![] };
    d.input_tables = Some(json!({ "samples": table }));
    // The bundle's FASTA also given as a param, and one input file filling two params.
    d.run.input_uris.push("s3://refs/hg38.fa".into());
    d.params["fasta"] = json!("s3://refs/hg38.fa");
    d.params["reads_again"] = json!("s3://bkt/in/reads.fq.gz");
    let c = build(&d, None, None, &[]);

    let file = entity(&c, sheet);
    assert_eq!(file["version"], "e1");
    assert_eq!(file["description"], "Samplesheet biffix wrote for `samples`");
    let run = entity(&c, "#run-run-1");
    assert!(ids(&run["object"]).contains(&sheet.to_string()));

    // One entity per file, its facts from each place it appears merged.
    let count =
        |id: &str| c["@graph"].as_array().unwrap().iter().filter(|e| e["@id"] == id).count();
    assert_eq!(count("s3://refs/hg38.fa"), 1);
    let fasta = entity(&c, "s3://refs/hg38.fa");
    assert_eq!(fasta["description"], "Reference bundle `grch38`: fasta");
    assert_eq!(fasta["exampleOfWork"]["@id"], "#param/fasta");
    // A file filling two params realizes both.
    assert_eq!(count("s3://bkt/in/reads.fq.gz"), 1);
    let mut both = ids(&entity(&c, "s3://bkt/in/reads.fq.gz")["exampleOfWork"]);
    both.sort();
    assert_eq!(both, ["#param/reads", "#param/reads_again"]);
}
