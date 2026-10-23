//! The resume rules: whether a run can be resumed, and by which submit ([`resume_target`],
//! [`resumable_now`]).

use biffix_workflow::engines::Engine;

use crate::store::registry::RegisteredVersion;
use crate::store::RunDetailView;

/// What a resumed run inherits from the run it resumes, once the prior run has passed the checks.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResumeTarget {
    /// The prior run's output prefix: the resumed run writes to the same work dir and cache.
    pub(crate) output_prefix: String,
    /// Nextflow: the prior run's engine session, passed as `-resume <id>`. Snakemake has none; it
    /// resumes from the metadata the prior run synced into the shared prefix.
    pub(crate) session_id: Option<String>,
}

/// The resume rules: the version being submitted and the prior run use the same engine, it ran the
/// same workflow (name + project; any version — "fix a late step and resume" is the point) in the
/// project the new run launches in (`project`; a run launched before per-project routing ran in
/// the default project whatever its label), and the prior run itself is resumable
/// ([`resumable`]). An explicit `requested_prefix` must name the prior's prefix (a trailing `/`
/// aside). `Err` is the 400 message.
pub(super) fn resume_target(
    prior: &RunDetailView,
    version: &RegisteredVersion,
    projects: &crate::config::projects::Projects,
    project: &crate::config::projects::ProjectContext,
    requested_prefix: Option<&str>,
) -> Result<ResumeTarget, String> {
    let prior_id = &prior.run.run_id;
    let engine = version.version.engine;
    if prior.run.engine.as_deref() != Some(engine.as_str()) {
        let prior_engine = prior.run.engine.as_deref().map(|e| {
            e.parse::<Engine>().map_or_else(|_| e.to_string(), |e| engine_label(e).to_string())
        });
        return Err(match prior_engine {
            Some(prior_engine) => format!(
                "run {prior_id} is a {prior_engine} run, not a {} run",
                engine_label(engine)
            ),
            None => format!(
                "run {prior_id} records no engine, so it is not a {} run",
                engine_label(engine)
            ),
        });
    }
    if prior.run.workflow_name.as_deref() != Some(version.name.as_str())
        || prior.run.project.as_deref() != Some(version.project.as_str())
    {
        return Err(format!(
            "run {prior_id} is a run of {}/{}, not {}/{}",
            prior.run.project.as_deref().unwrap_or("?"),
            prior.run.workflow_name.as_deref().unwrap_or("?"),
            version.project,
            version.name
        ));
    }
    let target = resumable_into(prior, projects, project)?;
    if let Some(p) = requested_prefix.map(str::trim).filter(|p| !p.is_empty()) {
        if !crate::runs::output_prefix::same_output_prefix(p, &target.output_prefix) {
            return Err(format!(
                "output_prefix must equal the resumed run's ({})",
                target.output_prefix
            ));
        }
    }
    Ok(target)
}

/// Whether `prior` can be resumed by a submit now: [`resumable_into`] the project its version runs
/// in today. What `GET /runs/{id}` serves as `resumable` (and what its failure summary offers), so
/// the page offers resume exactly when a submit takes it. `Err` says why not.
pub(crate) fn resumable_now(
    prior: &RunDetailView,
    projects: &crate::config::projects::Projects,
) -> Result<ResumeTarget, String> {
    let label = prior.run.project.as_deref().unwrap_or_default();
    let project = projects
        .for_version(label)
        .map_err(|e| format!("run {} cannot be resumed: its version's {e}", prior.run.run_id))?;
    resumable_into(prior, projects, project)
}

/// [`resumable`], resumed by a run of `project`: the prior ran in that project
/// ([`crate::config::projects::Projects::for_run`], the one rule every reader of a run uses), and
/// its output prefix, which the resumed run is forced onto, is inside that project's bucket (a
/// prefix in a bucket no project owns any more is refused rather than written to).
fn resumable_into(
    prior: &RunDetailView,
    projects: &crate::config::projects::Projects,
    project: &crate::config::projects::ProjectContext,
) -> Result<ResumeTarget, String> {
    let prior_id = &prior.run.run_id;
    let ran_in = projects.for_run(prior);
    if ran_in.id != project.id {
        return Err(format!(
            "cannot resume across projects: run {prior_id} ran in project `{}`, and this version \
             runs in `{}`",
            ran_in.id, project.id
        ));
    }
    let target = resumable(prior)?;
    let root = format!("s3://{}/", project.bucket);
    if !project.bucket.is_empty() && !target.output_prefix.starts_with(&root) {
        return Err(format!(
            "run {prior_id} ran in {}, outside project `{}`'s bucket {root}, so it cannot be \
             resumed; launch it again instead",
            target.output_prefix, project.id
        ));
    }
    Ok(target)
}

/// The half of the resume rules that depends on the prior run alone: it has a known engine, it is
/// finished, its intermediate files were not deleted, a Nextflow run recorded its engine session,
/// and it recorded an output prefix to resume in (the resumed run is forced onto it verbatim, so it
/// finds the prior's work dir).
/// `Err` says why not.
fn resumable(prior: &RunDetailView) -> Result<ResumeTarget, String> {
    let prior_id = &prior.run.run_id;
    let Some(engine) = prior.run.engine.as_deref().and_then(|e| e.parse::<Engine>().ok()) else {
        return Err(format!("run {prior_id} records no known engine"));
    };
    if !crate::runs::detail::state_is_terminal(&prior.run.state) {
        return Err(format!(
            "run {prior_id} is still {}; only a finished run can be resumed",
            prior.run.state
        ));
    }
    if prior.work_deleted_at_micros.is_some() {
        return Err(format!(
            "run {prior_id}'s intermediate files were deleted, so it can no longer be resumed; \
             launch it again instead"
        ));
    }
    let session_id = prior.engine_session_id.clone();
    if engine == Engine::Nextflow && session_id.is_none() {
        return Err(format!(
            "run {prior_id} has no recorded Nextflow session (it predates resume support)"
        ));
    }
    let output_prefix = prior
        .output_prefix
        .clone()
        .ok_or_else(|| format!("run {prior_id} has no output prefix to resume in"))?;
    Ok(ResumeTarget { output_prefix, session_id })
}

/// An engine's name as people write it.
pub(crate) fn engine_label(engine: Engine) -> &'static str {
    match engine {
        Engine::Snakemake => "Snakemake",
        Engine::Nextflow => "Nextflow",
    }
}
