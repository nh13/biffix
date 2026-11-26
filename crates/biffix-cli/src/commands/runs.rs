//! `biffix runs submit|list|show|follow|cancel|experiment|rename`.

use std::collections::BTreeSet;
use std::time::Duration;

use biffix_core::ids::RunId;
use biffix_core::state::RunState;
use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::FormSchema;
use biffix_workflow::validate::{
    coerce_params, default_nextflow_outdir, validate_params, NEXTFLOW_OUTDIR_PARAM,
};

use biffix_api::{
    FailureSummary, RunDetail, RunFilter, RunSubmitRequest, SubmittedRun,
    FAILURE_SUMMARY_SETTLE_SECS,
};
use serde::Deserialize;

use crate::api::ApiClient;
use crate::error::{CliError, CliResult, TRANSIENT_BUDGET};
use crate::params;
use crate::render::terminal_safe;
use crate::render::OutputMode;

use super::Session;

const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Most re-asks of a pending failure summary while following (one per poll interval): enough to
/// outlast the window the server may say `pending` for.
const SETTLE_POLLS: u32 = FAILURE_SUMMARY_SETTLE_SECS.div_ceil(POLL_INTERVAL.as_secs()) as u32;

/// The terminal outcome the CLI maps to a follow exit code, from the run's projected state.
fn terminal_of(state: &str) -> Option<RunState> {
    state.parse::<RunState>().ok().filter(|s| s.is_terminal())
}

/// How following a run ends, once the server says it is `terminal` or its `state` is one this CLI
/// knows to be: exit 0/8/9 for the known ones, and for a terminal state this CLI does not know
/// (the run-state set is open) an unclassified error naming it. `None` while it is in flight.
fn follow_outcome(run_id: &str, state: &str, terminal: bool) -> Option<CliResult<()>> {
    match terminal_of(state) {
        Some(known) => Some(terminal_result(run_id, known)),
        None if terminal => Some(Err(CliError::Unclassified(format!(
            "run {run_id} ended in state {state:?}, which this biffix does not know; see `biffix \
             runs show {run_id}`"
        )))),
        None => None,
    }
}

/// The follow result for a terminal run: exit 0 on success, 8 failed, 9 cancelled.
fn terminal_result(run_id: &str, state: RunState) -> CliResult<()> {
    match state {
        RunState::Succeeded => Ok(()),
        RunState::Failed => Err(CliError::RunFailed(format!("run {run_id} failed"))),
        RunState::Cancelled => Err(CliError::RunCancelled(format!("run {run_id} was cancelled"))),
        RunState::PreparingImages | RunState::Submitted | RunState::Running => {
            unreachable!("terminal_of returns terminal states only")
        }
    }
}

/// Which experiment a submit launches the run into (`--experiment` / `--no-experiment`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExperimentChoice {
    /// Neither flag: a fresh run is in none; a resume inherits the prior run's experiment when it
    /// is still open (live, in the version's project), as it inherits the prior run's params.
    #[default]
    Unset,
    /// `--experiment <id>`: this one (the server checks it).
    Into(String),
    /// `--no-experiment`: none, even on a resume.
    Outside,
}

impl ExperimentChoice {
    /// The choice the two flags make (clap refuses both at once).
    pub(crate) fn of(experiment: Option<String>, no_experiment: bool) -> Self {
        match (experiment, no_experiment) {
            (Some(id), _) => Self::Into(id),
            (None, true) => Self::Outside,
            (None, false) => Self::Unset,
        }
    }
}

/// `--experiment ""` (or blank): refused, not read as "no experiment".
const BLANK_EXPERIMENT: &str =
    "--experiment: an experiment id must not be blank (use --no-experiment to launch outside any \
     experiment)";

pub struct SubmitOpts {
    pub version_id: String,
    pub params_file: Option<String>,
    pub inline: Vec<String>,
    /// Submit-time target override; `None` uses the version's registered target.
    pub target: Option<String>,
    /// Submit-time workflow output prefix (within the project bucket); `None` uses the isolated
    /// per-run default.
    pub output_prefix: Option<String>,
    /// Submit-time default task retry count; `None` uses the deploy's default.
    pub retries: Option<u32>,
    /// Submit-time keep-going; `None` uses the deploy's default.
    pub keep_going: Option<bool>,
    /// A finished run of the same workflow to resume (Nextflow or Snakemake); the server checks it
    /// is resumable.
    pub resume_from: Option<String>,
    /// A human name for the run.
    pub run_name: Option<String>,
    /// A reference bundle id; the server fills the form's reference params from it.
    pub reference: Option<String>,
    /// What happens to the run's intermediate files; `None` uses the deploy's default.
    pub retention: Option<biffix_core::event::Retention>,
    /// Images a task may name, pinned and imported at submit (`--image`). Empty on a resume: the
    /// prior run's pinned ones.
    pub images: Vec<String>,
    /// The experiment to launch the run into; the server checks it.
    pub experiment: ExperimentChoice,
    /// Run as one Batch job on one host (`--single-job`), with these overrides of the version's
    /// default queue-class and size; `None` runs one Batch job per task.
    pub single_job: Option<biffix_api::SingleJobRequest>,
    pub follow: bool,
}

/// Refuse a `--single-job` the version cannot take before the API is asked: a version that does not
/// allow it (`allowed` is its `single_job:`), or a size that is not one. The server decides the
/// rest (the deploy's queue-classes and instance types).
fn check_single_job(
    allowed: Option<&biffix_workflow::single_job::SingleJobSpec>,
    request: &biffix_api::SingleJobRequest,
) -> CliResult<()> {
    if allowed.is_none() {
        return Err(CliError::InvalidInput(
            "this workflow version does not allow single-job runs (its biffix.yaml has no \
             `single_job:`); drop --single-job"
                .into(),
        ));
    }
    biffix_workflow::single_job::SingleJobSpec::from(request)
        .size()
        .map(|_| ())
        .map_err(|e| CliError::InvalidInput(format!("--single-job: {e}")))
}

pub async fn submit(session: &mut Session, opts: SubmitOpts) -> CliResult<()> {
    if matches!(&opts.experiment, ExperimentChoice::Into(id) if id.trim().is_empty()) {
        return Err(CliError::InvalidInput(BLANK_EXPERIMENT.into()));
    }
    let client = session.authed_client().await?;
    // `--reference ""` is no bundle, as the server reads it.
    let reference = opts.reference.clone().filter(|r| !r.is_empty());

    // Resolve the version's FormSchema and validate params BEFORE calling the API, using the
    // SAME validator the server runs (shared via biffix-workflow) so a client-accept is never a
    // server-reject.
    let detail = client.get_workflow(&opts.version_id).await?;
    let form = FormSchema::deserialize(&detail.form_schema)
        .map_err(|e| CliError::Unclassified(format!("registered form_schema is invalid: {e}")))?;
    if let Some(request) = &opts.single_job {
        check_single_job(form.single_job.as_ref(), request)?;
    }

    // A resumed run starts from the params the prior run used (minus secrets, which were recorded
    // redacted); `--params` then `--param` override them, so "resume with one change" is one flag.
    // It runs the prior run's submit-time images, pinned, unless `--image` names others.
    // It is launched into the prior run's experiment too, unless a flag says otherwise.
    let (params, images, prior_experiment) = match &opts.resume_from {
        Some(prior_id) => {
            let prior = inherit_from(&client, prior_id, &form, opts.images.is_empty()).await?;
            let images = if opts.images.is_empty() { prior.images } else { opts.images };
            (prior.params, images, prior.experiment_id)
        }
        None => (serde_json::Map::new(), opts.images, None),
    };
    let experiment = match opts.experiment {
        ExperimentChoice::Into(id) => Some(id.trim().to_string()),
        ExperimentChoice::Outside => None,
        ExperimentChoice::Unset => match (opts.resume_from.as_deref(), prior_experiment) {
            (Some(prior_id), Some(id)) => {
                inherited_experiment(&client, prior_id, &id, &detail.project).await?
            }
            _ => None,
        },
    };
    let from_file = params::load_file(opts.params_file.as_deref().map(std::path::Path::new))?;
    for name in params::text_given_untyped(&from_file, &form.fields) {
        teprintln!(
            "[biffix] warning: the params file gives `{name}` as a number or boolean, but it takes \
             text; it is sent as the value YAML read (`1.10` reads as 1.1), so quote it to keep \
             the text"
        );
    }
    let params = params::assemble(params, from_file, &opts.inline, &form.fields)?;

    let errors =
        precheck(&form, &params, detail.engine == Engine::Nextflow.as_str(), reference.is_some())?;
    if !errors.is_empty() {
        // Report every failing field at once; never echo a value (the messages carry only field
        // names and generic reasons, so a secret value cannot leak).
        let joined = errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(CliError::InvalidInput(format!("parameter validation failed: {joined}")));
    }

    let req = RunSubmitRequest {
        experiment,
        retry_of: None,
        workflow_version_id: opts.version_id.into(),
        params: serde_json::Value::Object(params),
        target: opts.target,
        output_prefix: opts.output_prefix,
        retries: opts.retries,
        keep_going: opts.keep_going,
        resume_from: opts.resume_from.clone().map(RunId::new),
        run_name: opts.run_name,
        tables: None,
        reference,
        retention: opts.retention,
        images: (!images.is_empty()).then_some(images),
        single_job: opts.single_job,
    };
    let resp = client.submit_run(&req).await?;

    let resumed_from = opts.resume_from.as_deref();
    match session.output {
        OutputMode::Json if !opts.follow => {
            crate::render::print_json(&submitted_json(&resp, resumed_from));
        }
        OutputMode::Human if !opts.follow => tprintln!("{}", submitted_line(&resp, resumed_from)),
        _ => teprintln!("[biffix] {} — following…", submitted_line(&resp, resumed_from)),
    }

    if opts.follow {
        return follow_to_terminal(&client, resp.run_id.as_str(), session.output).await;
    }
    Ok(())
}

/// What a submit reports under `--output json`: the run and the state it was accepted in
/// (`submitted`, or `preparing_images` while its submit-time images are imported).
fn submitted_json(resp: &SubmittedRun, resumed_from: Option<&str>) -> serde_json::Value {
    let mut out = serde_json::json!({ "run_id": resp.run_id, "status": resp.state });
    if let Some(prior) = resumed_from {
        out["resumed_from"] = serde_json::json!(prior);
    }
    out
}

/// What a submit reports to a person: `submitted <run>`, noting a resume and a run that waits for
/// its images.
fn submitted_line(resp: &SubmittedRun, resumed_from: Option<&str>) -> String {
    let mut line = format!("submitted {}", resp.run_id);
    if let Some(prior) = resumed_from {
        line.push_str(&format!(" (resumes {prior})"));
    }
    if resp.state == RunState::PreparingImages.as_str() {
        line.push_str(" (preparing images)");
    }
    line
}

/// The placeholder a param the server fills itself stands in for during the pre-check.
const SERVER_FILLED: &str = "s3://server-resolved/";

/// Validate `params` as the server will, before submitting: on a copy with the params the server
/// fills itself standing in (a reference bundle's params when `with_reference`, a Nextflow
/// version's unset `outdir`), coerced as the server coerces them. A stand-in satisfies `required`
/// and reads as set for `show_if`, but it is not the value the run gets, so the errors for those
/// params (a pattern or a choice the placeholder cannot match) are the server's to judge and are
/// dropped. The request itself carries the params as given.
fn precheck(
    form: &FormSchema,
    params: &serde_json::Map<String, serde_json::Value>,
    nextflow: bool,
    with_reference: bool,
) -> CliResult<Vec<biffix_workflow::validate::ValidationError>> {
    let mut checked = params.clone();
    let mut server_filled: BTreeSet<String> = BTreeSet::new();
    // The server fills the bundle's params first (its URIs win over any given), and refuses a
    // bundle that cannot fill a required one.
    if with_reference {
        let Some(refs) = &form.references else {
            return Err(CliError::InvalidInput(
                "--reference: this workflow version takes no reference bundle".into(),
            ));
        };
        for param in refs.fill.keys() {
            checked.insert(
                param.clone(),
                serde_json::Value::String(format!("{SERVER_FILLED}reference")),
            );
            server_filled.insert(param.clone());
        }
    }
    let mut checked = serde_json::Value::Object(checked);
    // It then defaults an unset Nextflow `outdir` into the run's output prefix.
    if nextflow {
        let before = checked.get(NEXTFLOW_OUTDIR_PARAM).cloned();
        default_nextflow_outdir(&form.fields, &mut checked, Some(SERVER_FILLED));
        if checked.get(NEXTFLOW_OUTDIR_PARAM) != before.as_ref() {
            server_filled.insert(NEXTFLOW_OUTDIR_PARAM.to_string());
        }
    }
    let checked = coerce_params(&form.fields, checked);
    Ok(validate_params(&form.fields, &checked)
        .into_iter()
        .filter(|e| !server_filled.contains(&e.field))
        .collect())
}
/// What a `--resume-from` submit inherits from the prior run.
struct Inherited {
    params: serde_json::Map<String, serde_json::Value>,
    /// The prior run's submit-time images, each as spelled at the digest it ran.
    images: Vec<String>,
    /// The experiment the prior run is in, if any (checked to be open before it is inherited).
    experiment_id: Option<String>,
}

/// The experiment a resume of `prior_id` inherits: the prior run's `experiment_id` when it is still
/// open, a live experiment of the version's `project` (what the server accepts); else none, said
/// on stderr, so a resume still launches when that experiment was archived meanwhile.
async fn inherited_experiment(
    client: &ApiClient,
    prior_id: &str,
    experiment_id: &str,
    project: &str,
) -> CliResult<Option<String>> {
    let open = client.list_experiments(project).await?;
    match open.experiments.iter().find(|e| e.id == experiment_id) {
        Some(e) => {
            teprintln!("[biffix] inheriting experiment from {prior_id}: {} ({})", e.id, e.name);
            Ok(Some(e.id.clone()))
        }
        None => {
            teprintln!(
                "[biffix] not inherited: {prior_id}'s experiment {experiment_id} is archived or \
                 not in project {project}; launching outside any experiment (pass --experiment \
                 to choose one)"
            );
            Ok(None)
        }
    }
}

/// The images a resume of `prior` runs: its submit-time images as spelled at submit, pinned to the
/// digest each ran (`ubuntu:24.04@sha256:…`; the server reads them from its anchor, or, for a run
/// that never launched, from when it was accepted). The spelling is kept so tasks naming it
/// (`ubuntu:24.04`) still run that image; the digest so the same bytes run again.
fn inherited_images(prior: &RunDetail) -> Vec<String> {
    prior
        .submit_images
        .as_ref()
        .map(|images| images.iter().map(biffix_api::SubmittedImage::pinned_spelling).collect())
        .unwrap_or_default()
}

/// The params (and, unless `--image` names others, the submit-time images) a `--resume-from` submit
/// inherits from the prior run, reporting on stderr which were carried over and which must be
/// supplied again (names only — never a value).
async fn inherit_from(
    client: &ApiClient,
    prior_id: &str,
    form: &FormSchema,
    report_images: bool,
) -> CliResult<Inherited> {
    let prior = client.get_run(prior_id).await?;
    let images = inherited_images(&prior);
    if report_images && !images.is_empty() {
        teprintln!("[biffix] inheriting submit-time images from {prior_id}: {}", images.join(", "));
    }
    let inherited = params::inherit(&prior.params, &form.fields);
    let names = |keys: &mut dyn Iterator<Item = &String>| {
        keys.map(String::as_str).collect::<Vec<_>>().join(", ")
    };
    if !inherited.params.is_empty() {
        teprintln!(
            "[biffix] inheriting params from {prior_id}: {}",
            names(&mut inherited.params.keys())
        );
    }
    if !inherited.skipped_secrets.is_empty() {
        teprintln!(
            "[biffix] not inherited (secret; pass again with --param/--params): {}",
            names(&mut inherited.skipped_secrets.iter())
        );
    }
    if !inherited.skipped_undeclared.is_empty() {
        teprintln!(
            "[biffix] not inherited (not a param of this version): {}",
            names(&mut inherited.skipped_undeclared.iter())
        );
    }
    Ok(Inherited {
        params: inherited.params,
        images,
        experiment_id: prior.summary.experiment_id.clone(),
    })
}

pub(crate) async fn list(session: &mut Session, state: Option<RunFilter>) -> CliResult<()> {
    let client = session.authed_client().await?;
    let runs = client.list_runs(state).await?;
    match session.output {
        // The API's own shape, so every summary field (cost, outputs, resume lineage …) is there.
        OutputMode::Json => crate::render::print_json(&serde_json::json!(runs)),
        OutputMode::Human => {
            if runs.is_empty() {
                tprintln!("no runs");
            }
            for r in &runs {
                let progress = match (r.completed, r.total) {
                    (Some(c), Some(t)) => format!(" {c}/{t}"),
                    _ => String::new(),
                };
                tprintln!("{}  {}{progress}", r.run_id, r.state);
            }
        }
    }
    Ok(())
}

pub(crate) async fn show(session: &mut Session, run_id: &str) -> CliResult<()> {
    let client = session.authed_client().await?;
    show_with(&client, run_id, session.output).await
}

/// `biffix runs show`: the run's detail, and why it failed when it did.
async fn show_with(client: &ApiClient, run_id: &str, mode: OutputMode) -> CliResult<()> {
    let detail = client.get_run(run_id).await?;
    let why = failure_of(client, &detail).await;
    render_detail(&detail, why.as_ref(), Asked::Once, mode);
    Ok(())
}

pub async fn cancel(session: &mut Session, run_id: &str) -> CliResult<()> {
    let client = session.authed_client().await?;
    client.cancel_run(run_id).await?;
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "run_id": run_id, "status": "cancel_requested",
        })),
        OutputMode::Human => tprintln!("cancel requested for {run_id}"),
    }
    Ok(())
}

/// `biffix runs experiment <run> <experiment-id|--none>`: put the run in an experiment of its
/// project, move it to another, or (`None`) take it out.
pub async fn set_experiment(
    session: &mut Session,
    run_id: &str,
    experiment_id: Option<&str>,
) -> CliResult<()> {
    let experiment_id = match experiment_id.map(str::trim) {
        Some("") => {
            return Err(CliError::InvalidInput(
                "an experiment id must not be blank (use --none to take the run out of its \
                 experiment)"
                    .into(),
            ))
        }
        other => other,
    };
    let client = session.authed_client().await?;
    let run = client.set_run_experiment(run_id, experiment_id).await?;
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!(run)),
        OutputMode::Human => match &run.experiment_id {
            Some(id) => tprintln!("run {run_id} is in experiment {id}"),
            None => tprintln!("run {run_id} is in no experiment"),
        },
    }
    Ok(())
}

/// `biffix runs rename <run> <name|--clear>`: rename the run, or (`None`) clear its name.
pub async fn rename(session: &mut Session, run_id: &str, name: Option<&str>) -> CliResult<()> {
    let name = match name.map(str::trim) {
        Some("") => {
            return Err(CliError::InvalidInput(
                "a name must not be blank (use --clear to clear the run's name)".into(),
            ))
        }
        other => other,
    };
    let client = session.authed_client().await?;
    let run = client.rename_run(run_id, name).await?;
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!(run)),
        OutputMode::Human => match &run.run_name {
            Some(name) => tprintln!("run {run_id} is named {name}"),
            None => tprintln!(
                "run {run_id} has no name (the UI names it by its pipeline, date and samples)"
            ),
        },
    }
    Ok(())
}

pub async fn follow(session: &mut Session, run_id: &str) -> CliResult<()> {
    let client = session.authed_client().await?;
    follow_to_terminal(&client, run_id, session.output).await
}

/// How a failure summary was read, which says what a summary still `pending` tells the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// Once (`runs show`): asking again shortly may say more.
    Once,
    /// Again while it was pending, until it settled or the follow stopped waiting.
    UntilSettled,
}

fn render_detail(detail: &RunDetail, why: Option<&FailureSummary>, asked: Asked, mode: OutputMode) {
    match mode {
        // The JSON carries `pending` itself.
        OutputMode::Json => crate::render::print_json(&detail_json(detail, why)),
        OutputMode::Human => tprint!("{}", detail_text(detail, why, asked)),
    }
}

/// `GET /runs/{id}` as JSON: the full detail (params already secret-redacted server-side), not a
/// hand-picked subset that silently drops fields as the API grows. The one key the CLI adds is
/// `failure_summary` (`GET /runs/{id}/failure-summary`, in the API's own shape) for a failed run.
fn detail_json(detail: &RunDetail, why: Option<&FailureSummary>) -> serde_json::Value {
    let mut out = serde_json::json!(detail);
    if let (Some(why), Some(map)) = (why, out.as_object_mut()) {
        map.insert("failure_summary".into(), serde_json::json!(why));
    }
    out
}

/// The human rendering of a run's detail: its state, the executions it lost with their host, its
/// tasks, and why it failed when it did.
fn detail_text(detail: &RunDetail, why: Option<&FailureSummary>, asked: Asked) -> String {
    let s = &detail.summary;
    let mut out = terminal_safe(&format!("{}  {}", s.run_id, s.state), true) + "\n";
    if let Some(lost) = &detail.interruptions {
        out.push_str(&interruptions_line(lost));
    }
    for t in &detail.tasks {
        let mut notes = Vec::new();
        if t.attempts > 0 {
            notes.push(format!("attempt {}", t.attempts));
        }
        // AWS Batch ran its attempt more than once, or lost the host of its only run.
        if t.executions > 1 || t.lost_executions > 0 {
            notes.push(count(t.executions, "execution", "executions"));
            if t.lost_executions > 0 {
                let hosts = if t.lost_executions == 1 { "its host" } else { "their hosts" };
                notes.push(format!("{} lost with {hosts}", t.lost_executions));
            }
        }
        let notes =
            if notes.is_empty() { String::new() } else { format!(" ({})", notes.join(", ")) };
        let line = terminal_safe(&format!("  {}  {}{notes}", t.task_key.as_str(), t.state), true);
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(why) = why {
        out.push_str(&failure_text(why, s.run_id.as_str(), asked));
    }
    out
}

/// `n` and the noun for it, singular or plural.
fn count(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A run's interruptions as one line (ends with a newline), e.g.
/// `interruptions: 3 executions lost (2 spot reclaims, 1 host lost), 26 min lost`. No time is
/// given when none is known (AWS reported no start for any of them).
fn interruptions_line(lost: &biffix_api::RunInterruptions) -> String {
    let hosts = (lost.lost_executions - lost.spot_reclaims).max(0);
    let mut why = Vec::new();
    if lost.spot_reclaims > 0 {
        why.push(count(lost.spot_reclaims, "spot reclaim", "spot reclaims"));
    }
    if hosts > 0 {
        why.push(count(hosts, "host lost", "hosts lost"));
    }
    let why = if why.is_empty() { String::new() } else { format!(" ({})", why.join(", ")) };
    let time = if lost.lost_seconds <= 0.0 {
        String::new()
    } else if lost.lost_seconds < 60.0 {
        ", under a minute lost".to_string()
    } else {
        format!(", {} min lost", (lost.lost_seconds / 60.0).round())
    };
    format!(
        "interruptions: {} lost{why}{time}\n",
        count(lost.lost_executions, "execution", "executions")
    )
}

/// The human rendering of run `run_id`'s failure summary, as the run page words it (ends with a
/// newline). One still `pending` says it may be incomplete, and what to do (as `asked` allows).
fn failure_text(why: &FailureSummary, run_id: &str, asked: Asked) -> String {
    let mut out = String::from("why it failed:\n");
    for item in &why.items {
        out.push_str(&format!("  {}\n", item_headline(item)));
        out.push_str(&format!("    {}\n", terminal_safe(&item.advice, true)));
        // AWS's reason is the evidence only when there is no log; before launch it is the message.
        if let Some(reason) = &item.reason {
            if item.log_tail.is_empty() && item.stage.is_none() {
                out.push_str(&format!("    AWS says: {}\n", terminal_safe(reason, true)));
            }
        }
        for line in &item.log_tail {
            out.push_str(&format!("    | {}\n", terminal_safe(line, true)));
        }
    }
    let more = why.failed_steps - why.items.len() as i64;
    if more > 0 {
        let steps = if more == 1 { "step" } else { "steps" };
        out.push_str(&format!("  {more} more failed {steps}\n"));
    }
    if let Some(c) = &why.support {
        let url = c.url.as_deref().map(|u| format!(", {u}")).unwrap_or_default();
        out.push_str(&terminal_safe(&format!("  ask: {} <{}>{url}", c.name, c.email), true));
        out.push('\n');
    }
    if why.pending {
        let note = match asked {
            Asked::Once => format!(
                "  this may be incomplete: the engine's last output may not be in yet. Run `biffix \
                 runs show {run_id}` again shortly, or `biffix runs follow {run_id}`, which waits \
                 for it."
            ),
            Asked::UntilSettled => "  this may be incomplete: the engine's last output was still \
                                    not in when biffix stopped waiting."
                .to_string(),
        };
        out.push_str(&terminal_safe(&note, true));
        out.push('\n');
    }
    out
}

/// "<step>[ (<task key>)] failed: <message>[ · after N tries]" of one failure item.
fn item_headline(item: &biffix_api::FailureItem) -> String {
    let key = match &item.task_key {
        Some(key) if key.as_str() != item.step => format!(" ({})", key.as_str()),
        _ => String::new(),
    };
    let tries = if item.attempts > 1 {
        format!(" · after {} tries", item.attempts)
    } else {
        String::new()
    };
    terminal_safe(&format!("{}{key} failed: {}{tries}", item.step, item.message), true)
}

/// "<step> failed: <message>" of the summary's first item, for the follow error line.
fn failure_headline(why: &FailureSummary) -> Option<String> {
    why.items
        .first()
        .map(|item| terminal_safe(&format!("{} failed: {}", item.step, item.message), true))
}

/// Why `detail`'s run failed; `None` for a run that did not fail, an empty summary, or a summary
/// that could not be read (a warning on stderr: the run's own detail is still shown).
async fn failure_of(client: &ApiClient, detail: &RunDetail) -> Option<FailureSummary> {
    if terminal_of(&detail.summary.state) != Some(RunState::Failed) {
        return None;
    }
    match client.get_failure_summary(detail.summary.run_id.as_str()).await {
        Ok(why) if why.items.is_empty() => None,
        Ok(why) => Some(why),
        Err(e) => {
            teprintln!("{}", summary_read_warning(detail.summary.run_id.as_str(), &e));
            None
        }
    }
}

/// The stderr warning that `run_id`'s failure summary could not be read (`error`). The run ID
/// and the error can carry server text, so the line is printed terminal-safe.
fn summary_read_warning(run_id: &str, error: &dyn std::fmt::Display) -> String {
    terminal_safe(&format!("[biffix] warning: could not read why {run_id} failed: {error}"), false)
}

/// As [`failure_of`], re-asked every `interval` while the server says it is pending (at most
/// [`SETTLE_POLLS`] times); the last answer read is returned, and whether it waited (asked again
/// at all). Says once on stderr (human mode) that it is waiting for the engine's last output.
async fn settled_failure(
    client: &ApiClient,
    detail: &RunDetail,
    mode: OutputMode,
    interval: Duration,
) -> (Option<FailureSummary>, bool) {
    let mut why = failure_of(client, detail).await;
    let mut waited = false;
    for poll in 0..SETTLE_POLLS {
        if !why.as_ref().is_some_and(|w| w.pending) {
            break;
        }
        if poll == 0 && mode == OutputMode::Human {
            teprintln!("[biffix] waiting for the engine's last output…");
        }
        tokio::time::sleep(interval).await;
        waited = true;
        match failure_of(client, detail).await {
            Some(next) => why = Some(next),
            // A re-ask that fails keeps the answer already read.
            None => break,
        }
    }
    (why, waited)
}

/// Poll until the run reaches a terminal state, then map that to the exit code (0/8/9). Transient
/// errors are retried within a budget and never reported as a run failure. A failed run's reason
/// is shown, and named in the error.
async fn follow_to_terminal(client: &ApiClient, run_id: &str, mode: OutputMode) -> CliResult<()> {
    follow_with(client, run_id, mode, POLL_INTERVAL).await
}

/// [`follow_to_terminal`], polling every `interval`.
async fn follow_with(
    client: &ApiClient,
    run_id: &str,
    mode: OutputMode,
    interval: Duration,
) -> CliResult<()> {
    let mut transient = 0u32;
    let mut last_state = String::new();
    loop {
        match client.get_run(run_id).await {
            Ok(detail) => {
                transient = 0;
                say_state(run_id, &detail.summary.state, &mut last_state, mode);
                if let Some(outcome) =
                    follow_outcome(run_id, &detail.summary.state, detail.summary.terminal)
                {
                    // Asks nothing for a run that did not fail.
                    let (why, waited) = settled_failure(client, &detail, mode, interval).await;
                    // Show the run as it was when the summary settled, not before the wait (a
                    // detail that cannot be read again is shown as it was read).
                    let detail = match waited {
                        true => client.get_run(run_id).await.unwrap_or(detail),
                        false => detail,
                    };
                    say_state(run_id, &detail.summary.state, &mut last_state, mode);
                    // End as that run did (a cancellation may have folded meanwhile), keeping the
                    // ending first read should it no longer read as ended; say why it failed only
                    // while it still did.
                    let outcome =
                        follow_outcome(run_id, &detail.summary.state, detail.summary.terminal)
                            .unwrap_or(outcome);
                    let why = why
                        .filter(|_| terminal_of(&detail.summary.state) == Some(RunState::Failed));
                    render_detail(&detail, why.as_ref(), Asked::UntilSettled, mode);
                    return match why.as_ref().and_then(failure_headline) {
                        Some(headline) => {
                            outcome.map_err(|e| e.map_message(|m| format!("{m}: {headline}")))
                        }
                        None => outcome,
                    };
                }
            }
            Err(e) => {
                if let Some(stop) = poll_failed(run_id, e, &mut transient) {
                    return Err(stop);
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Says on stderr (human mode) that the followed run is now in `state`, when that is not
/// `last_state` (which it becomes). The state is the server's, so it is printed terminal-safe.
fn say_state(run_id: &str, state: &str, last_state: &mut String, mode: OutputMode) {
    if state == last_state {
        return;
    }
    if mode == OutputMode::Human {
        teprintln!("[biffix] {}", terminal_safe(&format!("{run_id} → {state}"), false));
    }
    *last_state = state.to_string();
}

/// A failed poll of `run_id` while following it, with `transient` such failures in a row before it:
/// `None` to poll again, or the error to stop with. A transient blip is polled through (within
/// [`TRANSIENT_BUDGET`]), not reported as the run failing; the first says so. Once the budget is
/// spent the run is likely still going, so the error says how to pick it up again. A refusal is
/// returned as it is.
fn poll_failed(run_id: &str, e: CliError, transient: &mut u32) -> Option<CliError> {
    if !e.is_transient() {
        return Some(e);
    }
    if *transient < TRANSIENT_BUDGET {
        if *transient == 0 {
            teprintln!("[biffix] {run_id}: {e}; retrying");
        }
        *transient += 1;
        return None;
    }
    Some(e.map_message(|m| {
        format!(
            "stopped following {run_id}, which may still be running: {m}; follow it again with \
             `biffix runs follow {run_id}`"
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_read_warning_is_terminal_safe() {
        let warning = summary_read_warning("run-\u{1b}[2J1", &"bad\u{7} gateway\r");
        assert_eq!(warning, "[biffix] warning: could not read why run-[2J1 failed: bad gateway");
    }

    #[test]
    fn single_job_is_refused_for_a_version_without_it_or_a_bad_size() {
        let allowed = biffix_workflow::single_job::SingleJobSpec::default();
        let request = biffix_api::SingleJobRequest::default();
        assert!(check_single_job(Some(&allowed), &request).is_ok());
        let err = check_single_job(None, &request).unwrap_err();
        assert!(err.to_string().contains("does not allow single-job runs"), "{err}");
        let bad = biffix_api::SingleJobRequest { instance: Some("huge".into()), ..request };
        let err = check_single_job(Some(&allowed), &bad).unwrap_err();
        assert!(err.to_string().contains("--single-job: "), "{err}");
    }

    fn form(json: serde_json::Value) -> FormSchema {
        serde_json::from_value(json).unwrap()
    }

    fn params(json: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        json.as_object().unwrap().clone()
    }

    /// An nf-core style reference form: `fasta` (a pattern the placeholder cannot match) filled
    /// from a bundle, a required `outdir` with a pattern, a patterned `label` no bundle fills, and
    /// a number.
    fn reference_form() -> FormSchema {
        form(serde_json::json!({
            "fields": [
                {"name": "fasta", "widget": "s3_file", "required": true, "is_output": false,
                 "is_secret": false, "is_path_input": true,
                 "rules": [{"pattern": "^\\S+\\.fn?a(sta)?(\\.gz)?$", "message": "must be a FASTA"}]},
                {"name": "outdir", "widget": "s3_dir", "required": true, "is_output": true,
                 "is_secret": false, "is_path_input": false,
                 "rules": [{"pattern": "^s3://my-bucket/", "message": "must be in my-bucket"}]},
                {"name": "label", "widget": "text", "required": false, "is_output": false,
                 "is_secret": false, "is_path_input": false,
                 "rules": [{"pattern": "^[a-z]+$", "message": "lower-case letters only"}]},
                {"name": "threads", "widget": "number", "required": false, "is_output": false,
                 "is_secret": false, "is_path_input": false}
            ],
            "warnings": [],
            "references": {"fill": {"fasta": "fasta"}, "required": ["fasta"], "label": "Genome"}
        }))
    }

    fn failing(errors: &[biffix_workflow::validate::ValidationError]) -> Vec<&str> {
        let mut fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        fields.sort();
        fields
    }

    #[test]
    fn the_precheck_leaves_server_filled_params_to_the_server() {
        let form = reference_form();
        // With a bundle and an unset `outdir`, both are the server's to fill: their rules are not
        // checked against placeholders, while `required` is satisfied.
        let errors = precheck(&form, &params(serde_json::json!({})), true, true).unwrap();
        assert_eq!(errors, []);
        // A param the server does not fill is still checked.
        let given = params(serde_json::json!({"label": "Not OK"}));
        assert_eq!(failing(&precheck(&form, &given, true, true).unwrap()), ["label"]);
        // Without a bundle, `fasta` is the submitter's (required), and an `outdir` they give is
        // theirs to get right.
        let given = params(serde_json::json!({"outdir": "s3://other/x"}));
        assert_eq!(failing(&precheck(&form, &given, true, false).unwrap()), ["fasta", "outdir"]);
        // A number's string spelling passes (the server coerces it before validating); off
        // Nextflow, nothing defaults `outdir`.
        let given = params(serde_json::json!({"fasta": "s3://b/g.fa", "threads": "8"}));
        assert_eq!(failing(&precheck(&form, &given, false, false).unwrap()), ["outdir"]);
    }

    #[test]
    fn a_reference_is_refused_for_a_form_without_a_bundle() {
        let form = form(serde_json::json!({"fields": [], "warnings": []}));
        let err = precheck(&form, &serde_json::Map::new(), false, true).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("takes no reference bundle")),
            "{err:?}"
        );
    }

    #[test]
    fn terminality_follows_the_core_run_states() {
        assert_eq!(terminal_of("succeeded"), Some(RunState::Succeeded));
        assert_eq!(terminal_of("failed"), Some(RunState::Failed));
        assert_eq!(terminal_of("cancelled"), Some(RunState::Cancelled));
        for open in ["preparing_images", "running", "submitted", "paused-by-a-newer-server"] {
            assert_eq!(terminal_of(open), None, "{open}");
        }
        let code = |state| terminal_result("r", state).unwrap_err().exit_code();
        assert_eq!(code(RunState::Failed), 8);
        assert_eq!(code(RunState::Cancelled), 9);
        assert!(terminal_result("r", RunState::Succeeded).is_ok());
    }

    /// A run the server reports terminal in a state this CLI does not know ends the follow, as a
    /// classified error naming the state, rather than polling forever.
    #[tokio::test]
    async fn following_ends_when_the_server_says_the_run_is_terminal() {
        let app = axum::Router::new().route(
            "/runs/{id}",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "run_id": "run-x", "state": "archived-by-a-newer-server", "terminal": true,
                    "tasks_observed": 0, "tasks_succeeded": 0, "tasks_running": 0,
                    "tasks_queued": 0, "tasks_failed": 0, "tasks_cached": 0, "outputs_total": 0,
                    "outputs_reused": 0, "input_uris": [], "tasks": [], "params": {},
                    "dag": {"nodes": [], "edges": []},
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ApiClient::new(
            reqwest::Client::new(),
            &base,
            crate::secret::Secret::new("tok".into()),
            false,
        );
        let err = tokio::time::timeout(
            Duration::from_secs(10),
            follow_to_terminal(&client, "run-x", OutputMode::Json),
        )
        .await
        .expect("a terminal run ends the follow")
        .unwrap_err();
        assert!(
            matches!(&err, CliError::Unclassified(m) if m.contains("archived-by-a-newer-server")),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 1);
        // A known terminal state keeps its own exit code; an unknown in-flight one keeps polling.
        assert_eq!(follow_outcome("r", "failed", true).unwrap().unwrap_err().exit_code(), 8);
        assert!(follow_outcome("r", "paused-by-a-newer-server", false).is_none());
    }

    fn response(state: &str) -> SubmittedRun {
        serde_json::from_value(serde_json::json!({"run_id": "run-x", "state": state})).unwrap()
    }

    #[test]
    fn a_submit_reports_the_state_the_run_was_accepted_in() {
        let preparing = response("preparing_images");
        assert_eq!(
            submitted_json(&preparing, Some("run-p")),
            serde_json::json!({"run_id": "run-x", "status": "preparing_images", "resumed_from": "run-p"})
        );
        assert_eq!(submitted_line(&preparing, None), "submitted run-x (preparing images)");
        let launched = response("submitted");
        assert_eq!(
            submitted_json(&launched, None),
            serde_json::json!({"run_id": "run-x", "status": "submitted"})
        );
        assert_eq!(submitted_line(&launched, Some("run-p")), "submitted run-x (resumes run-p)");
    }

    #[test]
    fn a_resume_inherits_the_prior_run_s_pinned_submit_images() {
        let detail = |submit_images: serde_json::Value| -> RunDetail {
            serde_json::from_value(serde_json::json!({
                "run_id": "run-p", "state": "failed", "tasks_observed": 0, "tasks_succeeded": 0,
                "tasks_running": 0, "tasks_queued": 0, "tasks_failed": 0, "tasks_cached": 0,
                "outputs_total": 0, "outputs_reused": 0, "input_uris": [],
                "tasks": [], "params": {}, "dag": {"nodes": [], "edges": []},
                "submit_images": submit_images,
            }))
            .unwrap()
        };
        assert!(inherited_images(&detail(serde_json::Value::Null)).is_empty());
        // A run that failed while preparing has no provenance, only the images it was given.
        // Each as spelled at submit (not normalized), at the digest it ran; one given with a
        // digest is given as-is.
        let prior = detail(serde_json::json!([
            {"declared_ref": "ubuntu:24.04",
             "source_ref": "docker.io/library/ubuntu:24.04@sha256:d"},
            {"declared_ref": "quay.io/b/y:2@sha256:e", "source_ref": "quay.io/b/y:2@sha256:e"},
            {"declared_ref": "quay.io/b/z", "source_ref": "quay.io/b/z:latest@sha256:f"}
        ]));
        assert!(prior.provenance.is_none());
        assert_eq!(
            inherited_images(&prior),
            ["ubuntu:24.04@sha256:d", "quay.io/b/y:2@sha256:e", "quay.io/b/z@sha256:f"]
        );
    }

    #[test]
    fn a_follow_that_runs_out_of_retries_says_how_to_follow_again() {
        let mut transient = 0;
        for _ in 0..TRANSIENT_BUDGET {
            assert!(
                poll_failed("run-x", CliError::Unavailable("503".into()), &mut transient).is_none()
            );
        }
        let err =
            poll_failed("run-x", CliError::Unavailable("503".into()), &mut transient).unwrap();
        assert!(
            matches!(&err, CliError::Unavailable(m)
                if m.contains("may still be running") && m.contains("biffix runs follow run-x")),
            "{err:?}"
        );
        // A refusal stops at once, as it is.
        let mut transient = 0;
        let err = poll_failed("run-x", CliError::NotFound("unknown run".into()), &mut transient);
        assert!(matches!(err, Some(CliError::NotFound(m)) if m == "unknown run"));
    }

    fn failure(json: serde_json::Value) -> FailureSummary {
        serde_json::from_value(json).unwrap()
    }

    /// One failure item: `step` failed with `message`, and the given extra fields.
    fn item(step: &str, message: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut item = serde_json::json!({
            "step": step, "cause": "unknown", "try_again": "resume", "message": message,
            "advice": "Do something.", "attempts": 1, "log_tail": [],
        });
        item.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        item
    }

    #[test]
    fn a_failure_reads_as_each_item_s_step_message_advice_and_log() {
        // The engine's own failure: its log lines, and no "AWS says" beside a log.
        let engine = failure(serde_json::json!({"failed_steps": 0, "items": [item(
            "workflow engine",
            "the workflow engine stopped with an error before any step failed",
            serde_json::json!({
                "advice": "The engine's log, below, says why.",
                "reason": "Essential container in task exited",
                "log_tail": ["ERROR ~ Validation failed!", "* --input: the file does not exist"],
            }),
        )]}));
        assert_eq!(
            failure_text(&engine, "run-x", Asked::Once),
            "why it failed:\n  workflow engine failed: the workflow engine stopped with an error \
             before any step failed\n    The engine's log, below, says why.\n    | ERROR ~ \
             Validation failed!\n    | * --input: the file does not exist\n"
        );
        // A step with its task key and tries, and more failed steps than shown; who to ask.
        let steps = failure(serde_json::json!({
            "failed_steps": 4,
            "items": [item("align", "it ran out of memory", serde_json::json!({
                "task_key": "align:sample=B", "attempts": 3,
            }))],
            "support": {"name": "Core", "email": "core@lab.example.org"},
        }));
        assert_eq!(
            failure_text(&steps, "run-x", Asked::Once),
            "why it failed:\n  align (align:sample=B) failed: it ran out of memory · after 3 \
             tries\n    Do something.\n  3 more failed steps\n  ask: Core <core@lab.example.org>\n"
        );
        // A task key that is its step's name is not repeated; one try is not counted.
        let same = failure(serde_json::json!({"failed_steps": 1, "items": [item(
            "world", "it failed", serde_json::json!({"task_key": "world"}),
        )]}));
        assert!(
            failure_text(&same, "run-x", Asked::Once).contains("  world failed: it failed\n"),
            "{}",
            failure_text(&same, "run-x", Asked::Once)
        );
        // Before launch: the reason is the message, never repeated as "AWS says".
        let before = failure(serde_json::json!({"failed_steps": 0, "items": [item(
            "image preparation", "image x could not be imported",
            serde_json::json!({"reason": "image x could not be imported", "stage": "image_preparation"}),
        )]}));
        assert_eq!(
            failure_text(&before, "run-x", Asked::Once),
            "why it failed:\n  image preparation failed: image x could not be imported\n    Do \
             something.\n"
        );
        // Never started: no log, so AWS's reason is the evidence.
        let never = failure(serde_json::json!({"failed_steps": 0, "items": [item(
            "workflow engine", "it never ran",
            serde_json::json!({"reason": "CannotPullContainerError: x not found"}),
        )]}));
        assert!(
            failure_text(&never, "run-x", Asked::Once)
                .contains("    AWS says: CannotPullContainerError: x not found\n"),
            "{}",
            failure_text(&never, "run-x", Asked::Once)
        );
        // A summary still settling says it may be incomplete, and what to do about it.
        let mut pending = engine.clone();
        pending.pending = true;
        let once = failure_text(&pending, "run-x", Asked::Once);
        assert!(
            once.ends_with(
                "  this may be incomplete: the engine's last output may not be in yet. Run `biffix \
                 runs show run-x` again shortly, or `biffix runs follow run-x`, which waits for \
                 it.\n"
            ),
            "{once}"
        );
        let waited = failure_text(&pending, "run-x", Asked::UntilSettled);
        assert!(
            waited.ends_with(
                "  this may be incomplete: the engine's last output was still not in when biffix \
                 stopped waiting.\n"
            ),
            "{waited}"
        );
        assert!(!failure_text(&engine, "run-x", Asked::Once).contains("incomplete"));
        assert_eq!(
            failure_headline(&engine).as_deref(),
            Some(
                "workflow engine failed: the workflow engine stopped with an error before any \
                 step failed"
            )
        );
        assert_eq!(
            failure_headline(&failure(serde_json::json!({"failed_steps": 0, "items": []}))),
            None
        );
    }

    #[test]
    fn a_log_line_cannot_drive_the_terminal() {
        // Every server-supplied string the human rendering prints is filtered, not only logs.
        const ESC: &str = "\u{1b}[2J\u{7}";
        let mut run = run_json("failed");
        run["run_id"] = serde_json::json!(format!("run-x{ESC}"));
        run["tasks"] = serde_json::json!([
            {"task_key": format!("align:sample={ESC}"), "state": format!("failed{ESC}"), "attempts": 1}
        ]);
        let detail: RunDetail = serde_json::from_value(run).unwrap();
        let why = failure(serde_json::json!({
            "failed_steps": 3,
            "pending": true,
            "items": [
                // A step with a log: its step, task key, message, advice and log lines.
                item(&format!("align{ESC}"), &format!("it broke{ESC}"), serde_json::json!({
                    "task_key": format!("align:sample=B{ESC}"),
                    "advice": format!("Do this{ESC}."),
                    "log_tail": [format!("a\tlog line{ESC}")],
                })),
                // No log: AWS's reason is printed.
                item("workflow engine", "it never ran", serde_json::json!({
                    "reason": format!("CannotPullContainerError{ESC}"),
                })),
            ],
            "support": {
                "name": format!("Core{ESC}"), "email": format!("c@x.org{ESC}"),
                "url": format!("https://x.org/{ESC}"),
            },
        }));
        let text = detail_text(&detail, Some(&why), Asked::Once);
        assert!(!text.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "{text:?}");
        for shown in [
            "run-x[2J  failed\n",
            "  align:sample=[2J  failed[2J (attempt 1)\n",
            "  align[2J (align:sample=B[2J) failed: it broke[2J\n",
            "    Do this[2J.\n",
            "    | a\tlog line[2J\n",
            "    AWS says: CannotPullContainerError[2J\n",
            "  ask: Core[2J <c@x.org[2J>, https://x.org/[2J\n",
            "`biffix runs show run-x[2J`",
        ] {
            assert!(text.contains(shown), "{shown:?} in {text:?}");
        }
        assert_eq!(failure_headline(&why).as_deref(), Some("align[2J failed: it broke[2J"));
    }

    #[test]
    fn following_waits_out_the_server_s_settle_window() {
        let window = Duration::from_secs(biffix_api::FAILURE_SUMMARY_SETTLE_SECS);
        assert!(POLL_INTERVAL * SETTLE_POLLS >= window);
    }

    /// A `GET /runs/{id}` answer for `run-x` in `state`.
    fn run_json(state: &str) -> serde_json::Value {
        serde_json::json!({
            "run_id": "run-x", "state": state, "terminal": state != "running",
            "tasks_observed": 0, "tasks_succeeded": 0, "tasks_running": 0,
            "tasks_queued": 0, "tasks_failed": 0, "tasks_cached": 0, "outputs_total": 0,
            "outputs_reused": 0, "input_uris": [], "tasks": [], "params": {},
            "dag": {"nodes": [], "edges": []},
        })
    }

    #[test]
    fn a_run_s_text_counts_the_executions_aws_batch_ran_again() {
        let mut run = run_json("succeeded");
        run["interruptions"] =
            serde_json::json!({"lost_executions": 3, "spot_reclaims": 2, "lost_seconds": 1559.0});
        run["tasks"] = serde_json::json!([
            {"task_key": "align", "state": "succeeded", "attempts": 1, "executions": 2,
             "lost_executions": 1},
            {"task_key": "sort", "state": "running", "attempts": 2, "executions": 3,
             "lost_executions": 2},
            {"task_key": "index", "state": "succeeded", "attempts": 1, "executions": 1},
            {"task_key": "stats", "state": "succeeded", "attempts": 1},
        ]);
        let detail: RunDetail = serde_json::from_value(run).unwrap();
        assert_eq!(
            detail_text(&detail, None, Asked::Once),
            "run-x  succeeded\n\
             interruptions: 3 executions lost (2 spot reclaims, 1 host lost), 26 min lost\n\
             \x20 align  succeeded (attempt 1, 2 executions, 1 lost with its host)\n\
             \x20 sort  running (attempt 2, 3 executions, 2 lost with their hosts)\n\
             \x20 index  succeeded (attempt 1)\n\
             \x20 stats  succeeded (attempt 1)\n"
        );
        let line = |lost, spot, seconds| {
            interruptions_line(&biffix_api::RunInterruptions {
                lost_executions: lost,
                spot_reclaims: spot,
                lost_seconds: seconds,
            })
        };
        assert_eq!(
            line(1, 1, 12.0),
            "interruptions: 1 execution lost (1 spot reclaim), under a minute lost\n"
        );
        assert_eq!(
            line(2, 0, 90.0),
            "interruptions: 2 executions lost (2 hosts lost), 2 min lost\n"
        );
        // No start reported for any: no time.
        assert_eq!(line(1, 0, 0.0), "interruptions: 1 execution lost (1 host lost)\n");
    }

    #[test]
    fn a_failed_run_s_json_adds_its_failure_summary() {
        let detail: RunDetail = serde_json::from_value(run_json("failed")).unwrap();
        let plain = serde_json::to_value(&detail).unwrap();
        assert_eq!(detail_json(&detail, None), plain);
        let why = failure(
            serde_json::json!({"failed_steps": 0, "items": [item("workflow engine", "x", serde_json::json!({}))]}),
        );
        let mut with = detail_json(&detail, Some(&why));
        assert_eq!(with["failure_summary"], serde_json::to_value(&why).unwrap());
        with.as_object_mut().unwrap().remove("failure_summary");
        assert_eq!(with, plain, "every RunDetail key as it is");
    }

    /// What a stub API answers and counts.
    #[derive(Default)]
    struct Stub {
        /// The run's state.
        state: String,
        /// The run's state from its second read on, when it changes (a fold landing late).
        later_state: Option<String>,
        /// Summary answers left that say `pending` (`u32::MAX`: always).
        pending: std::sync::atomic::AtomicU32,
        /// Answer `failure-summary` with a 500.
        broken: bool,
        /// Answer `failure-summary` with a 500 once it has answered this many times.
        broken_after: Option<u32>,
        summaries: std::sync::atomic::AtomicU32,
        /// `GET /runs/{id}` requests.
        runs: std::sync::atomic::AtomicU32,
    }

    /// A stub API for run `run-x`, and a client of it.
    async fn stub_api(stub: Stub) -> (ApiClient, Arc<Stub>) {
        use std::sync::atomic::Ordering::SeqCst;
        let stub = Arc::new(stub);
        let app = axum::Router::new()
            .route(
                "/runs/{id}",
                axum::routing::get(|axum::extract::State(s): axum::extract::State<Arc<Stub>>| async move {
                    let read = s.runs.fetch_add(1, SeqCst);
                    let state = match &s.later_state {
                        Some(later) if read > 0 => later,
                        _ => &s.state,
                    };
                    axum::Json(run_json(state))
                }),
            )
            .route(
                "/runs/{id}/failure-summary",
                axum::routing::get(|axum::extract::State(s): axum::extract::State<Arc<Stub>>| async move {
                    let asked = s.summaries.fetch_add(1, SeqCst);
                    if s.broken || s.broken_after.is_some_and(|n| asked >= n) {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
                    }
                    let pending = s.pending.load(SeqCst) > 0;
                    if pending && s.pending.load(SeqCst) != u32::MAX {
                        s.pending.fetch_sub(1, SeqCst);
                    }
                    let mut v = serde_json::json!({"failed_steps": 0, "items": [item(
                        "workflow engine", "the workflow engine stopped with an error before any step failed",
                        serde_json::json!({"log_tail": ["ERROR ~ boom"]}),
                    )]});
                    if pending {
                        v["pending"] = serde_json::json!(true);
                    }
                    axum::Json(v).into_response()
                }),
            )
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ApiClient::new(
            reqwest::Client::new(),
            &base,
            crate::secret::Secret::new("tok".into()),
            false,
        );
        (client, stub)
    }

    use axum::response::IntoResponse;
    use std::sync::Arc;

    #[tokio::test]
    async fn show_asks_why_only_a_failed_run_failed() {
        use std::sync::atomic::Ordering::SeqCst;
        let (client, stub) = stub_api(Stub { state: "failed".into(), ..Default::default() }).await;
        show_with(&client, "run-x", OutputMode::Json).await.unwrap();
        assert_eq!(stub.summaries.load(SeqCst), 1);
        let (client, stub) =
            stub_api(Stub { state: "succeeded".into(), ..Default::default() }).await;
        show_with(&client, "run-x", OutputMode::Human).await.unwrap();
        assert_eq!(stub.summaries.load(SeqCst), 0);
        // A summary that cannot be read still shows the run.
        let (client, stub) =
            stub_api(Stub { state: "failed".into(), broken: true, ..Default::default() }).await;
        show_with(&client, "run-x", OutputMode::Human).await.unwrap();
        // A 500 is not retried: asked once.
        assert_eq!(stub.summaries.load(SeqCst), 1);
    }

    #[tokio::test]
    async fn following_a_failed_run_waits_for_its_summary_to_settle_and_names_why() {
        use std::sync::atomic::Ordering::SeqCst;
        let interval = Duration::from_millis(10);
        let stub = Stub { state: "failed".into(), pending: 2.into(), ..Default::default() };
        let (client, stub) = stub_api(stub).await;
        let err = follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!(stub.summaries.load(SeqCst), 3);
        // It waited, so the run's detail is read again to show beside the settled summary.
        assert_eq!(stub.runs.load(SeqCst), 2);
        assert!(
            matches!(&err, CliError::RunFailed(m)
                if m.starts_with("run run-x failed: ") && m.contains("workflow engine failed:")),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 8);
        // A summary that never settles is asked a bounded number of times.
        let stub = Stub { state: "failed".into(), pending: u32::MAX.into(), ..Default::default() };
        let (client, stub) = stub_api(stub).await;
        let err = follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!(stub.summaries.load(SeqCst), 1 + SETTLE_POLLS);
        assert_eq!(err.exit_code(), 8);
        // A summary that is settled at once is shown with the detail already read.
        let (client, stub) = stub_api(Stub { state: "failed".into(), ..Default::default() }).await;
        follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!((stub.summaries.load(SeqCst), stub.runs.load(SeqCst)), (1, 1));
        // A re-ask that fails keeps the summary already read.
        let stub = Stub {
            state: "failed".into(),
            pending: u32::MAX.into(),
            broken_after: Some(1),
            ..Default::default()
        };
        let (client, stub) = stub_api(stub).await;
        let err = follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!(stub.summaries.load(SeqCst), 2);
        assert!(
            matches!(&err, CliError::RunFailed(m) if m.contains("workflow engine failed:")),
            "{err:?}"
        );
        // Another ending asks nothing more.
        let (client, stub) =
            stub_api(Stub { state: "cancelled".into(), ..Default::default() }).await;
        let err = follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!(err.exit_code(), 9);
        assert_eq!(stub.summaries.load(SeqCst), 0);
    }

    #[tokio::test]
    async fn following_ends_as_the_run_read_after_its_summary_settled() {
        use std::sync::atomic::Ordering::SeqCst;
        let interval = Duration::from_millis(10);
        // A cancellation folds while the summary settles: the run ends cancelled, not failed, and
        // is not said to have failed.
        let stub = Stub {
            state: "failed".into(),
            later_state: Some("cancelled".into()),
            pending: 1.into(),
            ..Default::default()
        };
        let (client, stub) = stub_api(stub).await;
        let err = follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!(stub.runs.load(SeqCst), 2);
        assert!(
            matches!(&err, CliError::RunCancelled(m) if m == "run run-x was cancelled"),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 9);
        // A re-read in flight again (not a state the API should give) keeps the ending first read.
        let stub = Stub {
            state: "failed".into(),
            later_state: Some("running".into()),
            pending: 1.into(),
            ..Default::default()
        };
        let (client, _) = stub_api(stub).await;
        let err = follow_with(&client, "run-x", OutputMode::Json, interval).await.unwrap_err();
        assert_eq!(err.exit_code(), 8);
    }
}
