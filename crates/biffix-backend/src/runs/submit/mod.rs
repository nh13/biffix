//! The submit pipeline: the transport-independent steps of launching a run, for a submit request
//! ([`request::submit_run`], behind `POST /runs`) and the launch sweep.
//!
//! - [`output_prefix_override`]: the rules for a caller-chosen output prefix.
//! - [`input_candidates`] + [`check_inputs`]: which inputs to existence-check, and the bounded,
//!   concurrent S3 probes that check them.
//! - [`submit`]: validate params against the frozen form → mint the run
//!   session → build the `run_submitted` anchor → launch the coordinator. It validates the params
//!   on its own, so a caller that skipped the earlier checks still cannot launch on bad params
//!   (inputs are existence-checked where the request is settled, before anything is written).
//!
//! A submit request ([`request`]) does what needs the request's context first: image readiness
//! (and the images given at submit, [`images`]), the resume rules ([`resume`]), the
//! reference-bundle and samplesheet fills ([`tables`]), pruning the params a `show_if` hides, and
//! writing the samplesheets, or parking a run whose images are not ready ([`park`]); secret params
//! are redacted before any event carries them ([`redaction`]);
//! [`crate::runs::launch::launch_run`] writes the image map, calls [`submit`] and records the
//! events.
//!
//! The AWS steps go through [`crate::aws::CloudProvider`], so this is unit-testable with a fake.

pub(crate) mod images;
pub(crate) mod park;
pub(crate) mod redaction;
pub(crate) mod request;
pub(crate) mod resume;
pub(crate) mod tables;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use futures::StreamExt;

use biffix_core::event::RunEvent;
use biffix_core::ids::{CoordinatorJobId, RunId};
use biffix_core::s3_layout::split_s3_uri;
use biffix_workflow::formschema::FormField;
use biffix_workflow::samples::{SheetRow, SheetSpec};

use crate::aws::{
    batch::{CoordinatorLaunch, LaunchVariant},
    AwsError, CloudProvider,
};
use crate::register::workflow::{WorkflowSource, WorkflowVersion};
use crate::runs::launch::LaunchPlan;
use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::FormSchema;
use biffix_workflow::validate::{
    coerce_params, default_nextflow_outdir, input_paths_to_check, validate_params, ValidationError,
};
use redaction::redact_secret_params;

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("workflow version contains an invalid FormSchema: {0}")]
    InvalidFormSchema(#[source] serde_json::Error),
    /// Each refused field as `field: message`, joined with `; ` (what the API answers, and what a
    /// parked run's launch records as its failure reason).
    #[error(
        "validation failed: {}",
        .0.iter().map(|e| format!("{}: {}", e.field, e.message)).collect::<Vec<_>>().join("; ")
    )]
    Validation(Vec<ValidationError>),
    #[error("input(s) missing in S3: {}", .0.join(", "))]
    MissingInputs(Vec<String>),
    #[error("aws: {0}")]
    Aws(#[from] crate::aws::AwsError),
    /// Launching the coordinator failed in a way that may have started it (SubmitJob's answer was
    /// lost: see [`crate::aws::batch::BatchJobs::live_coordinators`]), and no job of the run could
    /// be found: it may be running, unbound to the run, or not exist.
    #[error("launching the coordinator failed in a way that may have started it: {0}")]
    LaunchUncertain(String),
}

/// A launch: the request as submit settled it ([`LaunchPlan`], also what a run waiting for its
/// images is parked as), and what is resolved at launch beside it.
pub struct SubmitRequest {
    pub run_id: RunId,
    pub version: WorkflowVersion,
    /// Where the coordinator fetches the immutable workflow source from (workflow-level metadata).
    pub source: WorkflowSource,
    /// Everything the run was asked for: params, inputs, target, prefix, retries, the images given
    /// at submit (their keys are in `image_map`), … (its `version_id` is `version`'s).
    pub plan: LaunchPlan,
    /// The deploy's coordinator queue, as it is at launch.
    pub coordinator_queue: String,
    /// The project the run launches in (its version's): its bucket, from which the per-run
    /// default output prefix is derived (empty in a local composition with no bucket: then no
    /// prefix is recorded), its id (the jobs' project tag) and its run role.
    pub project: crate::aws::batch::LaunchProject,
    /// The coordinator variant to launch on (`crate::images::variants::launch_variant`), on the
    /// anchor.
    pub variant: LaunchVariant,
    /// When `version.image_digest` is the default image derived with the engine runtime: the
    /// digest it was derived from (the imported image), recorded on the anchor.
    pub image_source_digest: Option<String>,
    /// The run's image map and where it was written (`crate::images::map`); recorded on the anchor
    /// and handed to the coordinator.
    pub image_map: Option<(String, std::collections::BTreeMap<String, String>)>,
}

#[derive(Debug)]
pub struct SubmitOutcome {
    /// The run_submitted anchor event to append (the provenance anchor).
    pub run_submitted: RunEvent,
    /// The coordinator binding event: pairs this run with its coordinator's `batch_job_id` so the
    /// fold can attribute the AWS coordinator stream (which carries no `run_id`). Without it the
    /// run never leaves SUBMITTED, because terminality is anchored on coordinator exit.
    pub(crate) coordinator_submitted: RunEvent,
    pub assumed_role_arn: String,
    pub coordinator_job_id: String,
}

/// The key prefixes biffix keeps for itself in a project bucket, each with what it holds. A run's
/// output prefix inside one would mix its files with biffix's: other runs' files, staged workflow
/// sources, or a cache the cache-expiry sweep deletes (`biffix_ingest::cache_expiry`) with the
/// run's results in it.
const RESERVED_NAMESPACES: [(&str, &str); 3] = [
    (biffix_core::s3_layout::RUNS_PREFIX, "where biffix keeps each run's own files"),
    (
        biffix_core::s3_layout::CACHE_PREFIX,
        "where biffix keeps its caches, which it deletes as they expire",
    ),
    (crate::aws::objects::SOURCE_PREFIX, "where biffix stages workflow sources"),
];

/// A submit-time output prefix override, normalized: trimmed and ending in `/` (the coordinator
/// appends `work`, `.nextflow/cache`, … to it directly, so `s3://b/results` would put the work dir
/// at `s3://b/resultswork`). It must lie in the project bucket (the run role can write only there)
/// and outside every namespace biffix keeps for itself ([`RESERVED_NAMESPACES`]: `biffix/runs/`,
/// `biffix/cache/`, `workflow-sources/`). `Ok(None)` for a blank value (the per-run default); `Err`
/// is the 400 message, naming the namespace.
pub(crate) fn output_prefix_override(
    requested: Option<&str>,
    bucket: &str,
) -> Result<Option<String>, String> {
    let Some(p) = requested.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let normalized = if p.ends_with('/') { p.to_string() } else { format!("{p}/") };
    let root = format!("s3://{bucket}/");
    if bucket.is_empty() || !normalized.starts_with(&root) {
        return Err(format!("output_prefix must be within the project bucket {root}"));
    }
    // S3 keys are literal, but the engines and tools writing under the prefix may normalize a
    // path, so `./`, `..` or `//` could name the runs namespace (or leave the bucket) after the
    // check below; refuse them, as the other S3-path checks do. The bucket root itself is fine.
    let key = &normalized[root.len()..];
    if !key.is_empty()
        && crate::config::data_sources::has_unplain_segment(key.strip_suffix('/').unwrap_or(key))
    {
        return Err(format!("output_prefix {normalized} has an empty, `.` or `..` path segment"));
    }
    for (namespace, holds) in RESERVED_NAMESPACES {
        let reserved = format!("{root}{namespace}");
        if normalized.starts_with(&reserved) {
            return Err(format!("output_prefix must not be inside {reserved}, {holds}"));
        }
    }
    Ok(Some(normalized))
}

/// How an input's existence is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputKind {
    /// An object: a HEAD, whose version id / ETag is its provenance tag.
    File,
    /// A folder (`WidgetHint::S3Dir`): a ListBucket probe for a key under `<uri>/`.
    Folder,
    /// Either (nf-schema `format: path`, a sheet column that takes a folder): a HEAD, then the
    /// folder probe when no object is there.
    FileOrFolder,
}

/// An input to existence-check before launch ([`input_candidates`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputCandidate {
    pub(crate) uri: String,
    pub(crate) kind: InputKind,
}

/// The inputs to existence-check, each once, in first-seen order: the path params (outputs skipped;
/// a param in `sheet_params` points at the samplesheet biffix is about to write, so it is skipped
/// too) and the file cells of each sheet biffix writes from rows. A URI named both with a strict
/// kind and as [`InputKind::FileOrFolder`] is checked by the strict one.
pub(crate) fn input_candidates<'a>(
    fields: &[FormField],
    params: &serde_json::Value,
    sheet_params: &BTreeSet<String>,
    sheets: impl IntoIterator<Item = (&'a SheetSpec, &'a [SheetRow])>,
) -> Vec<InputCandidate> {
    let accepts_folder: BTreeSet<&str> =
        fields.iter().filter(|f| f.accepts_folder).map(|f| f.name.as_str()).collect();
    let mut found: Vec<InputCandidate> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut add = |uri: String, kind: InputKind| match index.get(&uri) {
        Some(&i) => {
            if found[i].kind == InputKind::FileOrFolder {
                found[i].kind = kind;
            }
        }
        None => {
            index.insert(uri.clone(), found.len());
            found.push(InputCandidate { uri, kind });
        }
    };
    for p in input_paths_to_check(fields, params) {
        if sheet_params.contains(&p.field) {
            continue;
        }
        let kind = if p.is_prefix {
            InputKind::Folder
        } else if accepts_folder.contains(p.field.as_str()) {
            InputKind::FileOrFolder
        } else {
            InputKind::File
        };
        add(p.uri, kind);
    }
    for (spec, rows) in sheets {
        for row in rows {
            for c in spec.columns.iter().filter(|c| c.file) {
                let Some(v) = row.get(&c.name).map(|v| v.trim()).filter(|v| !v.is_empty()) else {
                    continue;
                };
                let kind = if c.accepts_folder { InputKind::FileOrFolder } else { InputKind::File };
                add(v.to_string(), kind);
            }
        }
    }
    found
}

/// The first candidate that is not a well-formed `s3://bucket/key` URI (a file needs a key; a
/// folder may be a bucket's root, `s3://bucket/`). Checked before any AWS call, since S3 rejects
/// it with an error a caller cannot tell from an outage.
pub(crate) fn malformed_input(candidates: &[InputCandidate]) -> Option<&str> {
    candidates
        .iter()
        .find(|c| {
            !split_s3_uri(&c.uri)
                .is_some_and(|(_, key)| c.kind != InputKind::File || !key.is_empty())
        })
        .map(|c| c.uri.as_str())
}

/// The URI to probe for a folder: `<uri>/`, so `s3://b/data` is not satisfied by `s3://b/data2/…`
/// (ListObjects matches any key that merely starts with the prefix). A bucket root stays as it is.
pub(crate) fn folder_probe(uri: &str) -> String {
    if split_s3_uri(uri).is_none_or(|(_, key)| key.is_empty()) || uri.ends_with('/') {
        uri.to_string()
    } else {
        format!("{uri}/")
    }
}

/// The URI to probe for an index named by its base path (`s3://b/refs/genome` for
/// `genome.amb`, `genome.bwt`, …): `<uri>.`, so the base matches only files named `<base>.<ext>`,
/// never `genome2/…`. A reference bundle may name an index this way
/// (`crate::api::references`), and submit accepts it for a param the bundle filled.
pub(crate) fn index_base_probe(uri: &str) -> String {
    format!("{uri}.")
}

/// How many existence checks run at once: thousands of samplesheet files are checked in seconds,
/// well inside the API's timeout, without opening a connection per file.
pub(crate) const INPUT_CHECK_CONCURRENCY: usize = 32;

/// Existence-check the candidates concurrently (at most [`INPUT_CHECK_CONCURRENCY`] at once): the
/// provenance tag of each one found (an object's version id / ETag, empty for a folder or an index
/// base path), by URI. A candidate in `index_bases` (a reference bundle's URIs) that is neither an
/// object nor a folder may be an index's base path ([`index_base_probe`]). A candidate absent from
/// the map is missing; an S3 error other than "absent" fails the whole check.
pub(crate) async fn check_inputs(
    cloud: &Arc<dyn CloudProvider>,
    candidates: &[InputCandidate],
    index_bases: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, crate::aws::AwsError> {
    let probe = |c: &InputCandidate| {
        let cloud = cloud.clone();
        let c = c.clone();
        let index_base = index_bases.contains(&c.uri);
        async move {
            let tag = match c.kind {
                InputKind::Folder => None,
                // A bucket root has no object to HEAD (S3 refuses an empty key).
                InputKind::FileOrFolder
                    if split_s3_uri(&c.uri).is_none_or(|(_, key)| key.is_empty()) =>
                {
                    None
                }
                InputKind::File | InputKind::FileOrFolder => cloud.head_object(&c.uri).await?,
            };
            let found = match (tag, c.kind) {
                (Some(tag), _) => Some(tag),
                (None, InputKind::File) => None,
                (None, _) => cloud.prefix_exists(&folder_probe(&c.uri)).await?.then(String::new),
            };
            let found = match found {
                None if index_base && !c.uri.ends_with('/') => {
                    cloud.prefix_exists(&index_base_probe(&c.uri)).await?.then(String::new)
                }
                found => found,
            };
            Ok::<_, crate::aws::AwsError>(found.map(|tag| (c.uri, tag)))
        }
    };
    let mut checks = futures::stream::iter(candidates.iter().map(probe))
        .buffer_unordered(INPUT_CHECK_CONCURRENCY);
    let mut provenance = BTreeMap::new();
    while let Some(outcome) = checks.next().await {
        if let Some((uri, tag)) = outcome? {
            provenance.insert(uri, tag);
        }
    }
    Ok(provenance)
}

/// The ECR repo URI the version's default worker image lives in, from its recorded coordinates.
/// `None` when it recorded none (the deploy's workflow repository is not an ECR URI); the
/// coordinator then uses the tenant workflow repository (`worker_image_repo`) as it is.
pub(crate) fn worker_image_repo_uri(version: &WorkflowVersion) -> Option<String> {
    match (&version.image_registry_id, &version.image_region, &version.image_repository) {
        (Some(registry_id), Some(region), Some(repository)) => {
            // China partitions use the `.com.cn` ECR domain; everything else `.com`.
            let domain =
                if region.starts_with("cn-") { "amazonaws.com.cn" } else { "amazonaws.com" };
            Some(format!("{registry_id}.dkr.ecr.{region}.{domain}/{repository}"))
        }
        _ => None,
    }
}

/// Run the submit pipeline. Its inputs were existence-checked when the request was settled
/// (`request::submit_run`, which refuses a missing one before anything is written); their
/// provenance tags are the plan's.
pub async fn submit(
    cloud: &Arc<dyn CloudProvider>,
    req: SubmitRequest,
) -> Result<SubmitOutcome, SubmitError> {
    // 1. Derive validation and redaction metadata from the immutable workflow version. Accepting
    // duplicate caller-supplied field metadata would allow a stale/malicious caller to omit a
    // secret flag and leak its value into the append-only event log.
    let form_schema = <FormSchema as serde::Deserialize>::deserialize(&req.version.form_schema)
        .map_err(SubmitError::InvalidFormSchema)?;

    // 2. Validate params against that frozen schema while values are still available. Boolean
    // params given as "true"/"false" become JSON booleans first, so the engine, the anchor and
    // every later view see a real boolean (Nextflow treats the string "false" as true).
    let mut req = req;
    // Resolve where the workflow's outputs land — the submit override, else the per-run default —
    // once, so the anchor records exactly the prefix the coordinator is launched with.
    let effective_output_prefix = crate::runs::output_prefix::effective_output_prefix(
        &req.project.bucket,
        req.run_id.as_str(),
        req.plan.output_prefix.as_deref(),
    );
    // (Submit has defaulted it already; a plan parked by an older biffix may not have. One the
    // form hid stays dropped: it is passed to no engine.)
    let outdir_hidden = req
        .plan
        .dropped_inactive
        .iter()
        .any(|p| p == biffix_workflow::validate::NEXTFLOW_OUTDIR_PARAM);
    if req.version.engine == Engine::Nextflow && !outdir_hidden {
        default_nextflow_outdir(
            &form_schema.fields,
            &mut req.plan.params,
            effective_output_prefix.as_deref(),
        );
    }
    req.plan.params = coerce_params(&form_schema.fields, req.plan.params);
    let errors = validate_params(&form_schema.fields, &req.plan.params);
    if !errors.is_empty() {
        return Err(SubmitError::Validation(errors));
    }

    // Resolve the target: a non-empty submit-time override wins, else the version's registered
    // target. Recorded on the anchor (below) and fed to the coordinator (via `CoordinatorLaunch`),
    // so one frozen version can build different targets across runs and each run stays
    // distinguishable in its provenance.
    let effective_target = req
        .plan
        .target
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| req.version.target.clone());

    // Nextflow only: the session the engine runs as — the resumed run's, else a fresh uuid minted
    // here so the anchor records it and the resume cache's location
    // (`<output_prefix>.nextflow/cache/<id>/`) is known before the engine starts. Snakemake has no
    // session to pin.
    let engine_session_id = (req.version.engine == Engine::Nextflow).then(|| {
        req.plan.engine_session_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    });

    // 4. Record the run's per-project role + mint its capability token (scoping is per-project).
    let session = cloud.mint_run_session(&req.run_id, &req.project).await?;

    // 5. Redact while the params are still local to the submit pipeline, then build the
    // run_submitted anchor. The event is the sole source for log, audit, provenance, and UI
    // projections, so no downstream consumer can recover a secret value.
    // The coordinator needs the real values to run; the log gets a redacted copy. (Secret
    // params in Batch env is a known limitation to revisit — deliver via the run's Secrets
    // Manager entry keyed by the capability token; the smoke path carries no secrets.)
    let launch_params = req.plan.params.clone();
    let event_params = redact_secret_params(&form_schema.fields, req.plan.params);
    let accepted_at = biffix_core::clock::epoch_secs();
    let anchor = biffix_core::event::RunSubmitted {
        // Who launched the run, and each input's provenance tag.
        submitter: Some(biffix_core::event::user_submitter(&req.plan.actor)),
        submitter_label: req.plan.submitter_label.clone(),
        input_provenance: (!req.plan.input_provenance.is_empty())
            .then(|| req.plan.input_provenance.clone()),
        target: effective_target.clone(),
        output_prefix: effective_output_prefix.clone(),
        // The session the coordinator is launched with, so the log names the resume cache.
        engine_session_id: engine_session_id.clone(),
        resumed_from: req.plan.resumed_from.as_ref().map(std::string::ToString::to_string),
        run_name: req.plan.run_name.clone(),
        dropped_inactive: (!req.plan.dropped_inactive.is_empty())
            .then(|| req.plan.dropped_inactive.clone()),
        input_tables: (!req.plan.input_tables.is_empty()).then(|| req.plan.input_tables.clone()),
        reference_bundle: req.plan.reference_bundle.clone(),
        sample_count: req.plan.sample_count,
        samples: req.plan.samples.clone(),
        retention: req.plan.retention.clone(),
        runtime: Some(biffix_core::event::RuntimeUse {
            engine_version: req.variant.engine_version.clone(),
            runtime_key: req.variant.runtime_key.clone(),
            coordinator_image: req.variant.image.clone(),
        }),
        image_source_digest: req.image_source_digest.clone(),
        image_map: req.image_map.as_ref().map(|(_, map)| map.clone()),
        submit_images: (!req.plan.images.is_empty()).then(|| req.plan.images.clone()),
        experiment_id: req.plan.experiment_id.clone(),
        ..biffix_core::event::RunSubmitted::new(
            req.version.id.as_str(),
            req.version.git_sha.as_str(),
            req.version.image_digest.as_str(),
            event_params,
            req.plan.input_uris.clone(),
        )
    };
    let run_submitted = biffix_core::event::run_submitted(
        &req.run_id,
        biffix_core::time::Timestamp::from_epoch_seconds(accepted_at),
        anchor,
    );

    // 6. Launch the coordinator.
    let launched = cloud
        .launch_coordinator(&CoordinatorLaunch {
            run_id: req.run_id.clone(),
            project: req.project.clone(),
            coordinator_queue: req.coordinator_queue.clone(),
            image_digest: req.version.image_digest.clone(),
            capability_token: session.capability_token.clone(),
            source: req.source.clone(),
            git_sha: req.version.git_sha.clone(),
            engine: req.version.engine,
            entrypoint: req.version.entrypoint.clone(),
            target: effective_target.clone(),
            output_prefix: effective_output_prefix.clone(),
            worker_image_repo_uri: worker_image_repo_uri(&req.version),
            params: launch_params,
            queue_classes: req.version.queue_classes.clone(),
            retries: req.plan.retries,
            keep_going: req.plan.keep_going,
            engine_session_id,
            resume: req.plan.resumed_from.is_some(),
            output_cache: (req.version.engine == Engine::Snakemake && form_schema.cache)
                .then(|| crate::runs::output_prefix::snakemake_cache_uri(&req.project.bucket))
                .flatten(),
            variant: req.variant.clone(),
            image_map_uri: req.image_map.as_ref().map(|(uri, _)| uri.clone()),
        })
        .await;
    let job = match launched {
        Ok(job) => job,
        // SubmitJob takes no idempotency token: an answer lost to a timeout, a dropped connection
        // or a 5xx may hide a job Batch created. The run's one live coordinator is that job (what
        // launched it earlier was stopped before this launch: `crate::runs::launch`); otherwise
        // whether one was created is unknown, and launching again would risk running the run
        // twice.
        Err(AwsError::Unavailable(reason)) => {
            match cloud.live_coordinators(&req.coordinator_queue, &req.run_id).await {
                Ok(jobs) if jobs.len() == 1 => {
                    tracing::warn!(run_id = %req.run_id, job_id = %jobs[0], error = %reason, "submit: SubmitJob's answer was lost; the job it created is the run's coordinator");
                    CoordinatorJobId::new(jobs[0].as_str())
                }
                _ => return Err(SubmitError::LaunchUncertain(reason)),
            }
        }
        Err(e) => return Err(e.into()),
    };

    // 7. Publish the coordinator binding now that SubmitJob has returned its id.
    let coordinator_submitted = biffix_core::event::coordinator_submitted(
        &req.run_id,
        biffix_core::time::Timestamp::from_epoch_seconds(accepted_at),
        &biffix_core::ids::BatchJobId::new(job.as_str()),
    );

    Ok(SubmitOutcome {
        run_submitted,
        coordinator_submitted,
        assumed_role_arn: session.run_role_arn,
        coordinator_job_id: job.as_str().to_string(),
    })
}

/// A submit the pipeline refused: invalid params or a missing input are the caller's, as are
/// params AWS finds too large to launch with; a stored form that does not parse is ours; any other
/// AWS failure is unavailable when retryable, else upstream.
impl From<SubmitError> for crate::error::ServiceError {
    fn from(err: SubmitError) -> Self {
        match err {
            e @ (SubmitError::Validation(_) | SubmitError::MissingInputs(_)) => {
                crate::error::ServiceError::Invalid(e.to_string())
            }
            SubmitError::InvalidFormSchema(e) => {
                crate::error::ServiceError::Internal(format!("stored form schema is invalid: {e}"))
            }
            SubmitError::Aws(crate::aws::AwsError::Invalid(m)) => {
                crate::error::ServiceError::Invalid(m)
            }
            SubmitError::Aws(e @ crate::aws::AwsError::Unavailable(_)) => {
                crate::error::ServiceError::Unavailable(format!("aws error: {e}"))
            }
            SubmitError::Aws(e) => crate::error::ServiceError::Upstream(format!("aws error: {e}")),
            e @ SubmitError::LaunchUncertain(_) => {
                crate::error::ServiceError::Unavailable(e.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_workflow::formschema::WidgetHint;
    use biffix_workflow::samples::SheetColumn;
    use serde_json::json;

    #[test]
    fn a_validation_failure_reads_as_its_fields_and_messages() {
        // The launch sweep records it as a parked run's failure reason, so it must read for people.
        let err = SubmitError::Validation(vec![
            biffix_workflow::validate::ValidationError {
                field: "reads".into(),
                message: "required".into(),
            },
            biffix_workflow::validate::ValidationError {
                field: "threads".into(),
                message: "must be at least 1".into(),
            },
        ]);
        let want = "validation failed: reads: required; threads: must be at least 1";
        assert_eq!(err.to_string(), want);
        assert_eq!(crate::error::ServiceError::from(err).to_string(), want);
    }

    #[test]
    fn the_runs_namespace_is_where_each_run_s_files_live() {
        assert_eq!(
            crate::runs::output_prefix::run_output_prefix("r1"),
            format!("{}r1/", biffix_core::s3_layout::RUNS_PREFIX)
        );
    }

    #[test]
    fn an_output_prefix_override_ends_in_a_slash_inside_the_bucket_and_outside_the_runs() {
        let ok = |p: &str| output_prefix_override(Some(p), "bkt");
        assert_eq!(ok("s3://bkt/results"), Ok(Some("s3://bkt/results/".into())));
        assert_eq!(ok(" s3://bkt/results/ "), Ok(Some("s3://bkt/results/".into())));
        assert_eq!(ok("s3://bkt"), Ok(Some("s3://bkt/".into())));
        assert_eq!(ok("  "), Ok(None));
        assert_eq!(output_prefix_override(None, "bkt"), Ok(None));
        for outside in ["s3://bkt-evil/x/", "s3://other/x/", "bkt/x/", "s3://bktx"] {
            let err = ok(outside).unwrap_err();
            assert!(err.contains("within the project bucket s3://bkt/"), "{outside}: {err}");
        }
        for inside in
            ["s3://bkt/biffix/runs/run-1/", "s3://bkt/biffix/runs", "s3://bkt/biffix/runs/"]
        {
            let err = ok(inside).unwrap_err();
            assert!(err.contains("must not be inside s3://bkt/biffix/runs/"), "{inside}: {err}");
        }
        // Every other namespace biffix keeps for itself is refused too, naming it; one that only
        // shares its start (`biffix/results/`, `biffix/cached/`) is the caller's.
        for (reserved, inside) in [
            ("s3://bkt/biffix/cache/", "s3://bkt/biffix/cache/snakemake/x/"),
            ("s3://bkt/biffix/cache/", "s3://bkt/biffix/cache/"),
            ("s3://bkt/biffix/cache/", "s3://bkt/biffix/cache"),
            ("s3://bkt/workflow-sources/", "s3://bkt/workflow-sources/v1/"),
            ("s3://bkt/workflow-sources/", "s3://bkt/workflow-sources"),
        ] {
            let err = ok(inside).unwrap_err();
            assert!(err.contains(&format!("must not be inside {reserved}")), "{inside}: {err}");
        }
        for theirs in ["s3://bkt/biffix/results/", "s3://bkt/biffix/cached/", "s3://bkt/biffix/"] {
            assert!(ok(theirs).is_ok(), "{theirs}");
        }
        // A path segment a normalizing consumer would rewrite is refused, wherever it is.
        for dotted in [
            "s3://bkt//biffix/runs/run-1/",
            "s3://bkt/./biffix/runs/run-1/",
            "s3://bkt/x/../biffix/runs/run-1/",
            "s3://bkt/..",
            "s3://bkt/results//",
            "s3://bkt//",
        ] {
            let err = ok(dotted).unwrap_err();
            assert!(err.contains("has an empty, `.` or `..` path segment"), "{dotted}: {err}");
        }
        // biffix's other prefixes are not a run's.
        assert!(ok("s3://bkt/biffix/results/").is_ok());
        assert_eq!(ok("s3://bkt/a.b/..c/"), Ok(Some("s3://bkt/a.b/..c/".into())));
        assert!(output_prefix_override(Some("s3://x/"), "").is_err());
    }

    #[test]
    fn a_folder_is_probed_below_its_own_slash() {
        assert_eq!(folder_probe("s3://b/data"), "s3://b/data/");
        assert_eq!(folder_probe("s3://b/data/"), "s3://b/data/");
        assert_eq!(folder_probe("s3://b/"), "s3://b/");
    }

    fn candidate(uri: &str, kind: InputKind) -> InputCandidate {
        InputCandidate { uri: uri.into(), kind }
    }

    #[test]
    fn a_file_needs_a_key_and_a_folder_may_be_a_bucket_root() {
        assert_eq!(malformed_input(&[candidate("s3://b/", InputKind::File)]), Some("s3://b/"));
        for ok in [InputKind::Folder, InputKind::FileOrFolder] {
            assert_eq!(malformed_input(&[candidate("s3://b/", ok)]), None);
        }
        for bad in ["foo", "s3://", "s3:///k", "s3://b"] {
            assert_eq!(malformed_input(&[candidate(bad, InputKind::Folder)]), Some(bad), "{bad}");
        }
        assert_eq!(malformed_input(&[candidate("s3://b/k", InputKind::File)]), None);
    }

    #[test]
    fn candidates_are_each_input_once_by_its_strictest_kind() {
        let path = |name: &str, widget: WidgetHint, accepts_folder: bool| FormField {
            name: name.into(),
            widget,
            is_path_input: true,
            accepts_folder,
            ..FormField::default()
        };
        let fields = vec![
            path("either", WidgetHint::S3File, true),
            path("file", WidgetHint::S3File, false),
            path("dir", WidgetHint::S3Dir, false),
            path("sheet", WidgetHint::Samplesheet, false),
            FormField { is_output: true, ..path("out", WidgetHint::S3Dir, false) },
        ];
        let params = json!({
            "either": "s3://b/shared",
            "file": "s3://b/shared",
            "dir": "s3://b/d",
            "sheet": "s3://b/biffix/runs/r/inputs/sheet.csv",
            "out": "s3://b/out/",
        });
        let spec = SheetSpec {
            columns: vec![
                SheetColumn { name: "fastq".into(), file: true, ..SheetColumn::default() },
                SheetColumn {
                    name: "index".into(),
                    file: true,
                    accepts_folder: true,
                    ..SheetColumn::default()
                },
                SheetColumn { name: "sample".into(), ..SheetColumn::default() },
            ],
            ..SheetSpec::default()
        };
        let row = |fastq: &str, index: &str| -> SheetRow {
            [("fastq", fastq), ("index", index), ("sample", "s3://b/not-a-file")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let rows = vec![row("s3://b/a.fq", "s3://b/idx"), row(" s3://b/a.fq ", "")];
        let sheet_params: BTreeSet<String> = ["sheet".to_string()].into();
        let found = input_candidates(&fields, &params, &sheet_params, [(&spec, rows.as_slice())]);
        assert_eq!(
            found,
            vec![
                candidate("s3://b/shared", InputKind::File),
                candidate("s3://b/d", InputKind::Folder),
                candidate("s3://b/a.fq", InputKind::File),
                candidate("s3://b/idx", InputKind::FileOrFolder),
            ]
        );
    }
}
