//! A submit request, from its checks to its launch ([`submit_run`]): the single path humans (and
//! later watchers) use to launch a run, behind `POST /runs`. Checks the request, resolves the frozen
//! version, fills the params (reference bundle, samplesheets, defaults) and prunes the ones the form
//! hides, validates them and existence-checks the inputs, and only then writes anything: the
//! samplesheets, then the launch ([`crate::runs::launch::launch_run`]: image map → submit pipeline
//! ([`crate::runs::submit`]: mint session → anchor → launch coordinator) → the `run_submitted`
//! anchor), so the run is immediately visible in the projection.
//!
//! Images given at submit (`images`) are checked with the request, then, once every other check
//! has passed, pinned and their imports (and derives) started or joined, before anything is
//! written. If every one is runnable the run launches as above; otherwise its launch request is
//! parked (`pending_launch`) and the run recorded as `preparing_images` (`run_preparing`), for the
//! sweep to launch once they are. A launch that cannot tell whether Batch created its coordinator
//! (`SubmitError::LaunchUncertain`) is parked the same way, for the sweep to check and launch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use biffix_api::{RunSubmitRequest, SubmittedRun};
use biffix_core::ids::RunId;

use super::images::{
    authorize_submit_images, pin_submit_images, prepare_submit_images, submit_image_refs,
};
use super::park::park;
use super::resume::resume_target;
use super::tables::{plan_tables, sheet_file_samples, sheet_uri};
use crate::auth::actor::AuthedActor;
use crate::config::projects::{ProjectContext, Projects};
use crate::error::ServiceError;
use crate::runs::launch::{
    ImagesReadiness, LaunchError, LaunchInputs, LaunchPlan, LAUNCH_PLAN_VERSION,
};
use crate::runs::submit::{check_inputs, input_candidates, malformed_input, SubmitError};
use crate::store::registry::RegisteredVersion;
use crate::store::StoreError;
use crate::AppState;
use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::FormSchema;
use biffix_workflow::validate::{
    coerce_params, default_nextflow_outdir, prune_inactive, validate_params, Pruned,
};

/// The largest submit-time retry count accepted.
const MAX_SUBMIT_RETRIES: u32 = 10;

/// The longest run name accepted.
pub(crate) const MAX_RUN_NAME_CHARS: usize = 200;

/// The 409 for a version `version_id` whose project the deploy does not configure: how to move it
/// into one.
pub(crate) fn unknown_version_project(
    e: &crate::config::projects::UnknownProject,
    version_id: &str,
) -> String {
    format!(
        "the version's {e}; re-register it into one of them \
         (`biffix workflows reregister {version_id} --project <project>`)"
    )
}

/// Why a run of `project` cannot read input `uri`: only other projects' runs can
/// ([`crate::config::projects::Projects::readable_only_by`]). `None` when it can, or `uri` is
/// outside every configured location.
pub(crate) fn unreadable_in(
    projects: &crate::config::projects::Projects,
    project: &crate::config::projects::ProjectContext,
    uri: &str,
) -> Option<String> {
    let owners = projects.readable_only_by(project, uri);
    if owners.is_empty() {
        return None;
    }
    let owners = owners.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ");
    Some(format!(
        "input {uri} is in storage of project {owners}, whose runs alone can read it; this \
         version runs in project `{}`",
        project.id
    ))
}

/// Submit a run for `actor` (`actor_id`, whom the caller has authorized to launch): launched, or
/// parked while its submit-time images are prepared (`state` says which).
///
/// The phases run in this order, each a cheap refusal before the next's AWS calls or writes:
/// [`resolve_version`] and [`check_request`] (the request's own checks), [`version_readiness`]
/// (the version's images and engine runtime), [`resolve_prior_runs`] (a resume or *Try again*,
/// and the output prefix's claim), [`fill_params`] (the params as the engine gets them, validated),
/// [`check_run_inputs`] (every input exists and is readable), the submit-time images, then
/// [`write_samplesheets`] and the launch (or park).
pub(crate) async fn submit_run(
    state: &AppState,
    actor: &AuthedActor,
    actor_id: String,
    req: RunSubmitRequest,
) -> Result<SubmittedRun, ServiceError> {
    // A run launched now, or parked to launch later, could not report its events.
    if state.capability.is_none() {
        return Err(ServiceError::Unavailable(crate::runs::launch::LAUNCH_DISABLED.into()));
    }
    // A human label for the run's submitter, snapshotted onto the immutable anchor.
    let submitter_label = crate::auth::actor::actor_label(state, actor).await;
    let bad = ServiceError::Invalid;

    let target = resolve_version(state, &req).await?;
    let mut req = req;
    let checked = check_request(state, &target, &mut req).await?;
    let ready = version_readiness(state, &target).await?;
    let prior = resolve_prior_runs(state, actor, &target, &req, checked.explicit_prefix).await?;
    let SubmitTarget { rec, projects, project } = &target;

    let form = <FormSchema as serde::Deserialize>::deserialize(&rec.version.form_schema)
        .map_err(|e| ServiceError::Internal(format!("stored form_schema invalid: {e}")))?;
    // The run id is minted now so a samplesheet written from rows lands under the run's own prefix.
    let run_id = RunId::new(format!("run-{}", uuid::Uuid::new_v4()));
    let effective_output_prefix = crate::runs::output_prefix::effective_output_prefix(
        &project.bucket,
        run_id.as_str(),
        prior.output_prefix.as_deref(),
    );
    // The retention sweep deletes only a run's own per-run work dir (`ingest::retention`), so
    // deleting on success cannot be honored in a custom prefix, or one resumed onto it.
    if req.retention == Some(biffix_core::event::Retention::DeleteOnSuccess)
        && !effective_output_prefix
            .as_deref()
            .is_some_and(|p| biffix_core::s3_layout::split_run_prefix(p).is_some())
    {
        return Err(bad(
            "retention delete_on_success applies only to a run in its own per-run prefix, not a \
             custom output_prefix (or a resume of a run in one)"
                .into(),
        ));
    }

    let filled =
        fill_params(state, &target, &form, &run_id, effective_output_prefix.as_deref(), &mut req)
            .await?;
    let inputs = check_run_inputs(state, &target, &form, &req.params, &filled).await?;

    // Every other check has passed: the images given at submit are pinned, authorized (an image
    // new to the tenant needs a Developer), and their imports (and derives) started or joined, so
    // no import starts for a request refused for another reason. One that will never be runnable
    // refuses the run before anything is written for it; one still being prepared parks it below.
    let pinned = pin_submit_images(state, checked.submit_refs).await?;
    authorize_submit_images(state, actor, &pinned).await?;
    let submit_statuses = prepare_submit_images(state, rec, &pinned).await?;
    let submit_readiness = crate::runs::launch::readiness(&submit_statuses);
    if let ImagesReadiness::Failed(reason) = &submit_readiness {
        return Err(ServiceError::Conflict(format!("this run cannot launch: {reason}")));
    }

    // The samples the run was launched with, for per-sample cost estimates: the first sheet (in the
    // form's field order) biffix writes from rows, else the sheet given as a file whose ids submit
    // can read ([`sheet_file_samples`]: the first sheet field given an `s3://` path, only).
    let first_sheet =
        match form.fields.iter().find_map(|f| filled.tables.iter().find(|t| t.param == f.name)) {
            Some(t) => Some((t.spec.sample_count(&t.rows), t.spec.sample_ids(&t.rows))),
            None => {
                sheet_file_samples(state.cloud.as_ref(), projects, project, &form, &req.params)
                    .await
            }
        };
    let (sample_count, sample_ids) = first_sheet.unzip();
    // The same sheet's sample ids, with the version's hint, for per-sample progress. Without a
    // sheet whose ids are known (none given, or a file that cannot be read) the set is open:
    // whatever the tasks name.
    let samples = {
        let spec = biffix_core::event::SampleSpec {
            ids: sample_ids.unwrap_or_default(),
            hint: form.samples.clone().unwrap_or_default(),
            reused: prior.resumed_samples,
        };
        (spec != biffix_core::event::SampleSpec::default()).then_some(spec)
    };
    let input_tables = write_samplesheets(state, filled.tables).await?;

    let plan = LaunchPlan {
        version: LAUNCH_PLAN_VERSION,
        version_id: rec.version.id.clone(),
        actor: actor_id,
        submitter_label,
        params: req.params.clone(),
        input_uris: inputs.uris,
        input_provenance: inputs.provenance,
        target: req.target.clone(),
        output_prefix: prior.output_prefix,
        retries: req.retries,
        keep_going: req.keep_going,
        engine_session_id: prior.resume.as_ref().and_then(|(_, t)| t.session_id.clone()),
        resumed_from: prior.resume.map(|(prior_id, _)| prior_id),
        run_name: checked.run_name,
        dropped_inactive: filled.dropped_inactive,
        input_tables,
        reference_bundle: filled.reference_bundle,
        sample_count,
        samples,
        retention: req.retention,
        images: pinned,
        experiment_id: checked.experiment_id,
    };

    let submitted = if let ImagesReadiness::Waiting(reasons) = submit_readiness {
        park(state, &run_id, &plan, &form, effective_output_prefix, reasons).await?
    } else {
        let launched = crate::runs::launch::launch_run(
            state,
            &run_id,
            &plan,
            LaunchInputs {
                project,
                source: &rec.source,
                version: ready.version,
                image_source_digest: ready.image_source_digest,
                variant: ready.variant,
                version_images: &ready.images,
                submit_images: &submit_statuses,
            },
        )
        .await;
        match launched {
            Ok(outcome) => SubmittedRun {
                run_id,
                state: biffix_core::state::RunState::Submitted.as_str().to_string(),
                coordinator_job_id: Some(outcome.coordinator_job_id.into()),
                assumed_role_arn: Some(outcome.assumed_role_arn),
            },
            // Whether Batch created the coordinator is unknown, so it is not launched again now:
            // the run is parked, and the launch sweep stops any coordinator this launch started
            // before it launches the run (`crate::runs::launch`).
            Err(LaunchError::Submit(SubmitError::LaunchUncertain(reason))) => {
                tracing::warn!(run_id = %run_id, error = %reason, "submit: the coordinator launch may or may not have happened; parking the run for the sweep");
                park(
                    state,
                    &run_id,
                    &plan,
                    &form,
                    effective_output_prefix,
                    vec![format!(
                        "launching its coordinator failed in a way that may have started it \
                         ({reason}); it is checked and launched again shortly"
                    )],
                )
                .await?
            }
            Err(e) => return Err(e.into()),
        }
    };
    read_and_dismiss_tried_again(state, &plan.actor, &prior.tried_again).await;
    Ok(submitted)
}

/// The frozen version a submit names and the project it runs in.
struct SubmitTarget {
    rec: RegisteredVersion,
    /// The deploy's projects, as read for this submit.
    projects: Arc<Projects>,
    /// The version's project: its bucket, roles and coordinator template.
    project: ProjectContext,
}

/// The version `req` names, in its project: a 400 for an unknown version, a 409 for one whose
/// project the deploy does not configure, a 400 for one whose staged files are outside it.
async fn resolve_version(
    state: &AppState,
    req: &RunSubmitRequest,
) -> Result<SubmitTarget, ServiceError> {
    let rec = state.registry.get_version(req.workflow_version_id.as_str()).await.map_err(|e| {
        ServiceError::reworded(e, |e| match e {
            StoreError::NotFound => {
                format!("unknown workflow version {}", req.workflow_version_id)
            }
            _ => e.to_string(),
        })
    })?;
    // The run launches in its version's project: its bucket, roles and coordinator template.
    // Read the projects again first if they lack the version's (a deploy may just have added it).
    let projects = state.projects.current_for(Some(&rec.project)).await;
    let project = projects
        .for_version(&rec.project)
        .map_err(|e| ServiceError::Conflict(unknown_version_project(&e, &rec.version.id)))?
        .clone();
    // The coordinator fetches the version's staged files from its project's bucket.
    if let Some(reason) = crate::register::pipeline::staged_outside_project(&rec, &project) {
        return Err(ServiceError::Invalid(reason));
    }
    Ok(SubmitTarget { rec, projects, project })
}

/// What the request's own checks settle.
struct CheckedRequest {
    run_name: Option<String>,
    experiment_id: Option<String>,
    /// A caller-chosen output prefix (normalized to end in `/`); `None` for a resume, whose prior
    /// run's prefix replaces it.
    explicit_prefix: Option<String>,
    /// The images given at submit, parsed.
    submit_refs: Vec<(String, biffix_workflow::images::ImageRef)>,
}

/// The request's own checks, before any AWS call: a malformed request is a cheap 400. Trims the
/// target and reads `null` params as none given, in `req`.
async fn check_request(
    state: &AppState,
    target: &SubmitTarget,
    req: &mut RunSubmitRequest,
) -> Result<CheckedRequest, ServiceError> {
    let bad = ServiceError::Invalid;
    let rec = &target.rec;
    // `Retention` is an open enum so a stored anchor with a newer value still parses, but a submit
    // may only ask for a value this backend acts on.
    if let Some(biffix_core::event::Retention::Unknown(value)) = &req.retention {
        return Err(bad(format!(
            "unknown retention `{value}` (default, delete_on_success or keep)"
        )));
    }
    // Only a Nextflow run's intermediate files (its `work/`) can be told apart from its results.
    if rec.version.engine != Engine::Nextflow
        && req.retention.as_ref().is_some_and(|r| *r != biffix_core::event::Retention::Default)
    {
        return Err(bad("retention applies only to Nextflow runs".into()));
    }
    // A retry count is a per-task multiplier on spend (each retry is a new Batch job), so cap it.
    if req.retries.is_some_and(|r| r > MAX_SUBMIT_RETRIES) {
        return Err(bad(format!("retries must be at most {MAX_SUBMIT_RETRIES}")));
    }
    let run_name = match req.run_name.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(name) if name.chars().count() > MAX_RUN_NAME_CHARS => {
            return Err(bad(format!("run_name must be at most {MAX_RUN_NAME_CHARS} characters")))
        }
        Some(name) => Some(name.to_string()),
    };
    // The target is one word of the engine's command line, after the coordinator's own options:
    // it must read as a target, never as an option (`--snakefile=…`) or more than one word.
    req.target = req.target.take().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if let Some(problem) = req.target.as_deref().and_then(|t| rec.version.engine.target_problem(t))
    {
        return Err(bad(format!("target: {problem}")));
    }
    // The experiment the run launches into: one of the version's project that is not archived.
    let experiment_id = match req.experiment.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(id) => {
            crate::runs::experiments::joinable_experiment(state, id, &rec.project, false)
                .await
                .map_err(|e| e.context("`experiment`"))?;
            Some(id.to_string())
        }
    };
    // A caller-chosen output prefix (normalized to end in `/`), checked now; a resume replaces it
    // with the prior run's.
    let explicit_prefix = if req.resume_from.is_some() {
        None
    } else {
        crate::runs::submit::output_prefix_override(
            req.output_prefix.as_deref(),
            &target.project.bucket,
        )
        .map_err(bad)?
    };
    // Params are an object of values by field name; `null` is read as none given.
    if req.params.is_null() {
        req.params = serde_json::Value::Object(Default::default());
    }
    if !req.params.is_object() {
        return Err(bad("params must be a JSON object".into()));
    }
    let submit_refs = submit_image_refs(state, req.images.take().unwrap_or_default())?;
    Ok(CheckedRequest { run_name, experiment_id, explicit_prefix, submit_refs })
}

/// The version as it is ready to run.
struct VersionReadiness {
    /// Every image the version declares, as it stands.
    images: Vec<crate::images::ImageStatus>,
    /// The version, its default image the runnable one.
    version: crate::register::workflow::WorkflowVersion,
    image_source_digest: Option<String>,
    /// The coordinator variant to launch on.
    variant: crate::aws::batch::LaunchVariant,
}

/// Whether the version can run now: a 409 naming what is not ready yet.
async fn version_readiness(
    state: &AppState,
    target: &SubmitTarget,
) -> Result<VersionReadiness, ServiceError> {
    let rec = &target.rec;
    // Every image the version declares must be imported into the tenant repository (and suit the
    // engine), and a Snakemake image derived with the engine runtime, before it runs.
    let images = crate::images::statuses(state, rec)
        .await
        .map_err(|e| ServiceError::Upstream(e.to_string()))?;
    let unready: Vec<String> =
        images.iter().filter_map(crate::images::ImageStatus::unready_reason).collect();
    if !unready.is_empty() {
        return Err(ServiceError::Conflict(format!(
            "this version cannot run yet: {}",
            unready.join("; ")
        )));
    }
    // Tasks run the default image as it is runnable (crate::runs::launch::runnable_default).
    let mut version = rec.version.clone();
    let image_source_digest = crate::runs::launch::runnable_default(&images, &mut version);

    // The coordinator variant (the version's pinned engine runtime on the base image) to launch
    // on. Resolved before anything is written for the run: a version whose runtime is not built
    // yet cannot be launched.
    let variant = crate::images::variants::launch_variant(state, &rec.version, &target.project)
        .await
        .map_err(|e| match e {
            crate::images::variants::NotRunnable::Error(e) => {
                tracing::warn!(error = %e, "submit: resolving the coordinator variant failed");
                ServiceError::from(e)
            }
            e => ServiceError::Conflict(e.to_string()),
        })?;
    Ok(VersionReadiness { images, version, image_source_digest, variant })
}

/// The runs a submit carries on from, and where it writes.
struct PriorRuns {
    /// The run resumed, and how.
    resume: Option<(RunId, super::resume::ResumeTarget)>,
    /// A resume carries the prior run's finished samples into its own grid
    /// (`SampleSpec::reused`).
    resumed_samples: Vec<biffix_core::event::ReusedSample>,
    /// The runs this one tries again, whose notifications are marked read for the caller once it
    /// is accepted: they no longer need the caller's attention.
    tried_again: Vec<String>,
    /// The run's output prefix: a resumed run is forced onto the prior run's (its work dir and
    /// cache live there; validated when that run was submitted, or the derived per-run default),
    /// else the caller's, else `None` ⇒ the isolated per-run default.
    output_prefix: Option<String>,
    /// Held until the submit returns, after the new run is recorded ([`claim_unused_prefix`]).
    _prefix_claim: Option<crate::store::Claim>,
}

/// Resolve a resume before any S3 call: an invalid one is a cheap 400. Seeing the prior run needs
/// ViewRuns (load_visible_run); an unknown or invisible prior run is a 404. A *Try again*'s run
/// likewise.
///
/// A run into a shared prefix (a resume's, or a caller-chosen one) holds a claim on it from the
/// check for an active run there until this submit returns (after the new run is recorded), so
/// two submits into one prefix cannot both pass the check.
async fn resolve_prior_runs(
    state: &AppState,
    actor: &AuthedActor,
    target: &SubmitTarget,
    req: &RunSubmitRequest,
    explicit_prefix: Option<String>,
) -> Result<PriorRuns, ServiceError> {
    let mut _prefix_claim = None;
    let mut resumed_samples = Vec::new();
    let resume = match &req.resume_from {
        None => None,
        Some(prior_id) => {
            let prior = visible_prior_run(state, actor, "resume_from", prior_id).await?;
            let resume = resume_target(
                &prior,
                &target.rec,
                &target.projects,
                &target.project,
                req.output_prefix.as_deref(),
            )
            .map_err(ServiceError::Invalid)?;
            // A resume waits for any run still using its prefix: a second resume of the same run,
            // or one of a run whose own resume is still going.
            _prefix_claim = Some(
                claim_unused_prefix(state, &resume.output_prefix, |active| {
                    format!(
                        "run {active} is still using {}; wait for it to finish before resuming run \
                         {prior_id}",
                        resume.output_prefix
                    )
                })
                .await?,
            );
            resumed_samples = crate::runs::detail::sample_grid_of(state, &prior)
                .await
                .map_err(|e| {
                    ServiceError::reworded(e, |e| format!("reading run {prior_id}'s samples: {e}"))
                })?
                .reused_by_resume();
            Some((prior_id.clone(), resume))
        }
    };
    // A *Try again* names the run it re-runs, which the caller must be able to see (like a
    // resume's prior run): an unknown or invisible one is a 404.
    let retry_of = match req.retry_of.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(id) => {
            let id = RunId::new(id);
            visible_prior_run(state, actor, "retry_of", &id).await?;
            Some(id)
        }
    };
    let tried_again: Vec<String> = req
        .resume_from
        .iter()
        .chain(retry_of.iter())
        .map(std::string::ToString::to_string)
        .collect();
    let output_prefix = match &resume {
        Some((_, t)) => Some(t.output_prefix.clone()),
        None => explicit_prefix,
    };
    // A caller-chosen prefix waits for any run still using it, as a resume's does.
    if let (None, Some(prefix)) = (&resume, &output_prefix) {
        _prefix_claim = Some(
            claim_unused_prefix(state, prefix, |active| {
                format!(
                    "run {active} is still using {prefix}; wait for it to finish, or choose another \
                     output_prefix"
                )
            })
            .await?,
        );
    }
    Ok(PriorRuns { resume, resumed_samples, tried_again, output_prefix, _prefix_claim })
}

/// The params as the engine gets them (in `req.params`), and what filling them settled.
struct FilledParams {
    /// The params (and rows) the form hides for these values, dropped.
    dropped_inactive: Vec<String>,
    /// The reference bundle chosen, with only the URIs it filled into params that survived.
    reference_bundle: Option<biffix_core::event::ReferenceUse>,
    /// The samplesheets biffix writes from rows.
    tables: Vec<super::tables::TablePlan>,
}

/// Fill the params as the engine will get them, then drop the ones the form hides for those
/// values: a `show_if` gate is judged on the same values validation sees, and a hidden input is
/// neither existence-checked, validated, nor passed to the engine. Validated now, before anything
/// is written for the run (the pipeline validates again).
async fn fill_params(
    state: &AppState,
    target: &SubmitTarget,
    form: &FormSchema,
    run_id: &RunId,
    effective_output_prefix: Option<&str>,
    req: &mut RunSubmitRequest,
) -> Result<FilledParams, ServiceError> {
    let bad = ServiceError::Invalid;
    let project = &target.project;
    // A chosen reference bundle fills the form's reference params (its URIs win over any given).
    let mut reference_bundle = match (req.reference.as_deref(), &form.references) {
        (None | Some(""), _) => None,
        (Some(_), None) => {
            return Err(bad("this workflow version takes no reference bundle".into()))
        }
        (Some(id), Some(refs)) => {
            let bundle = state.references.get_bundle(id).await.map_err(|e| match e {
                StoreError::NotFound => bad(format!("unknown reference bundle {id}")),
                other => ServiceError::from(other),
            })?;
            let resolved = crate::store::references::resolve(&bundle, refs).map_err(bad)?;
            if let Some(obj) = req.params.as_object_mut() {
                for (param, uri) in resolved.params {
                    obj.insert(param, serde_json::Value::String(uri));
                }
            }
            Some(resolved.used)
        }
    };
    // A param given as rows points at the samplesheet biffix writes for it (replacing any path also
    // given). Rows for a param without declared columns are refused below, unless it is hidden.
    let mut req_tables = req.tables.take().unwrap_or_default();
    if !project.bucket.is_empty() {
        if let Some(obj) = req.params.as_object_mut() {
            for param in req_tables.keys() {
                let spec = form
                    .fields
                    .iter()
                    .find(|f| &f.name == param)
                    .and_then(|f| f.samplesheet.as_ref());
                if let Some(spec) = spec {
                    let uri = sheet_uri(&project.bucket, run_id, param, spec);
                    obj.insert(param.clone(), serde_json::Value::String(uri));
                }
            }
        }
    }
    if target.rec.version.engine == Engine::Nextflow {
        default_nextflow_outdir(&form.fields, &mut req.params, effective_output_prefix);
    }
    let params = coerce_params(&form.fields, std::mem::take(&mut req.params));
    let Pruned { params, dropped: mut dropped_inactive, inactive } =
        prune_inactive(&form.fields, params);
    req.params = params;
    // Rows given for a param the form hides are dropped the same way: they are neither written as a
    // samplesheet nor passed to the engine.
    req_tables.retain(|param, _| {
        let hidden = inactive.contains(param);
        if hidden && !dropped_inactive.contains(param) {
            dropped_inactive.push(param.clone());
        }
        !hidden
    });
    // The bundle records only the URIs it filled into params that survived.
    if let (Some(used), Some(refs)) = (reference_bundle.as_mut(), &form.references) {
        used.uris.retain(|key, uri| {
            refs.fill.iter().any(|(param, k)| {
                k == key && req.params.get(param).and_then(|v| v.as_str()) == Some(uri.as_str())
            })
        });
    }
    let tables = plan_tables(form, req_tables, &project.bucket, run_id)?;
    let errors = validate_params(&form.fields, &req.params);
    if !errors.is_empty() {
        return Err(SubmitError::Validation(errors).into());
    }
    Ok(FilledParams { dropped_inactive, reference_bundle, tables })
}

/// The run's inputs, every one present.
struct CheckedInputs {
    uris: Vec<String>,
    /// Each input's version (its etag), as the existence check read it.
    provenance: BTreeMap<String, String>,
}

/// Existence-check every declared input, concurrently (bounded) so submit latency is a few round
/// trips, not one per input: path inputs (outputs skipped), each element of a `multiple` list,
/// and the rows' files. A malformed URI is a client error (400), rejected before any AWS
/// call; an S3 error other than "absent" is a 502; a missing input is a 400 before any write.
async fn check_run_inputs(
    state: &AppState,
    target: &SubmitTarget,
    form: &FormSchema,
    params: &serde_json::Value,
    filled: &FilledParams,
) -> Result<CheckedInputs, ServiceError> {
    let sheet_params: BTreeSet<String> = filled.tables.iter().map(|t| t.param.clone()).collect();
    let candidates = input_candidates(
        &form.fields,
        params,
        &sheet_params,
        filled.tables.iter().map(|t| (&t.spec, t.rows.as_slice())),
    );
    if let Some(uri) = malformed_input(&candidates) {
        return Err(ServiceError::Invalid(format!(
            "input is not a valid s3://bucket/key URI: {uri}"
        )));
    }
    // The backend can read every project's storage, so the existence check below no longer says
    // the run can: refuse what only another project's runs can read, naming that project.
    let bundle_uris =
        filled.reference_bundle.iter().flat_map(|b| b.uris.values().map(String::as_str));
    if let Some(reason) = candidates
        .iter()
        .map(|c| c.uri.as_str())
        .chain(bundle_uris)
        .find_map(|uri| unreadable_in(&target.projects, &target.project, uri))
    {
        return Err(ServiceError::Invalid(reason));
    }
    // A bundle may name an index by its base path, which the bundle's own check accepted.
    let index_bases: BTreeSet<String> =
        filled.reference_bundle.iter().flat_map(|b| b.uris.values().cloned()).collect();
    let provenance = check_inputs(&state.cloud, &candidates, &index_bases)
        .await
        .map_err(|e| ServiceError::Upstream(format!("s3 existence check failed: {e}")))?;
    let uris: Vec<String> = candidates.into_iter().map(|c| c.uri).collect();
    let missing: Vec<String> =
        uris.iter().filter(|u| !provenance.contains_key(*u)).cloned().collect();
    if !missing.is_empty() {
        return Err(SubmitError::MissingInputs(missing).into());
    }
    Ok(CheckedInputs { uris, provenance })
}

/// Every input exists and the params are valid: write the samplesheets (their params already
/// point at them), each recorded with what S3 stored.
async fn write_samplesheets(
    state: &AppState,
    tables: Vec<super::tables::TablePlan>,
) -> Result<BTreeMap<String, biffix_core::event::InputTable>, ServiceError> {
    let mut input_tables = BTreeMap::new();
    for t in tables {
        let etag = state
            .cloud
            .put_object(&t.uri, t.spec.render(&t.rows).into_bytes(), t.content_type())
            .await
            .map_err(|e| {
                ServiceError::Upstream(format!("writing the {} samplesheet failed: {e}", t.param))
            })?;
        input_tables.insert(
            t.param,
            biffix_core::event::InputTable {
                uri: t.uri,
                etag: (!etag.is_empty()).then_some(etag),
                rows: t.rows,
            },
        );
    }
    Ok(input_tables)
}

/// A claim on output prefix `prefix` for a submit into it, once no run is using it: two runs in
/// one prefix share its work dir (and, for Nextflow, one session's cache). A 409 when another submit
/// into it holds the claim, or (`in_use`'s message) when run `active` is still using it. Held until
/// the submit returns, after the new run is recorded.
async fn claim_unused_prefix(
    state: &AppState,
    prefix: &str,
    in_use: impl FnOnce(&str) -> String,
) -> Result<crate::store::Claim, ServiceError> {
    let claim = state
        .store
        .claim_output_prefix(prefix)
        .await
        .map_err(|e| ServiceError::reworded(e, |e| format!("claiming {prefix}: {e}")))?;
    let Some(claim) = claim else {
        return Err(ServiceError::Conflict(format!(
            "another submit into {prefix} is in progress; retry once it has finished"
        )));
    };
    let active = state.store.active_run_in_prefix(prefix).await.map_err(|e| {
        ServiceError::reworded(e, |e| format!("checking the runs using {prefix}: {e}"))
    })?;
    match active {
        Some(active) => Err(ServiceError::Conflict(in_use(active.as_str()))),
        None => Ok(claim),
    }
}

/// A prior run a submit names in `field` (`resume_from`, `retry_of`), which the caller must be able
/// to see (ViewRuns, `load_visible_run`). A refusal names the field and the run: an unknown or
/// invisible one is a 404, a caller who may not view runs a 403.
async fn visible_prior_run(
    state: &AppState,
    actor: &AuthedActor,
    field: &str,
    prior_id: &RunId,
) -> Result<crate::store::RunDetailView, ServiceError> {
    crate::runs::access::load_visible_run(state, actor, prior_id).await.map_err(|e| {
        let msg = match e {
            ServiceError::NotFound(_) => format!("`{field}`: unknown run {prior_id}"),
            ServiceError::Forbidden(_) | ServiceError::Unauthorized(_) => {
                format!("`{field}`: not allowed to view run {prior_id}")
            }
            _ => format!("`{field}`: could not load run {prior_id}"),
        };
        e.with_message(msg)
    })
}

/// Mark the notifications of the runs an accepted submit tries again read for `user`, so they leave
/// *Needs attention*: read, and dismissed (`via: try_again`; a read alone leaves them there).
/// Best-effort: the new run is already accepted, so a failure is logged (the user can still mark
/// them read, or dismiss them) rather than failing the submit.
async fn read_and_dismiss_tried_again(state: &AppState, user: &str, runs: &[String]) {
    if runs.is_empty() {
        return;
    }
    let selection = crate::store::notifications::NotificationSelection::Runs(runs.to_vec());
    if let Err(e) = state.store.mark_notifications_read(user, &selection).await {
        tracing::warn!(error = %e, runs = ?runs, "submit: marking the retried runs' notifications read failed");
    }
    let via = crate::store::notifications::DismissVia::TryAgain;
    if let Err(e) = state.store.dismiss_attention(user, runs, via).await {
        tracing::warn!(error = %e, runs = ?runs, "submit: dismissing the retried runs from needs-attention failed");
    }
}
