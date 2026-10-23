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

use biffix_api::{RunDetail, RunFilter, RunSubmitRequest, SubmittedRun};
use serde::Deserialize;

use crate::api::ApiClient;
use crate::error::{CliError, CliResult, TRANSIENT_BUDGET};
use crate::params;
use crate::render::OutputMode;

use super::Session;

const POLL_INTERVAL: Duration = Duration::from_secs(3);

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
    pub follow: bool,
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
        eprintln!(
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
    };
    let resp = client.submit_run(&req).await?;

    let resumed_from = opts.resume_from.as_deref();
    match session.output {
        OutputMode::Json if !opts.follow => {
            crate::render::print_json(&submitted_json(&resp, resumed_from));
        }
        OutputMode::Human if !opts.follow => println!("{}", submitted_line(&resp, resumed_from)),
        _ => eprintln!("[biffix] {} — following…", submitted_line(&resp, resumed_from)),
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
            eprintln!("[biffix] inheriting experiment from {prior_id}: {} ({})", e.id, e.name);
            Ok(Some(e.id.clone()))
        }
        None => {
            eprintln!(
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
        eprintln!("[biffix] inheriting submit-time images from {prior_id}: {}", images.join(", "));
    }
    let inherited = params::inherit(&prior.params, &form.fields);
    let names = |keys: &mut dyn Iterator<Item = &String>| {
        keys.map(String::as_str).collect::<Vec<_>>().join(", ")
    };
    if !inherited.params.is_empty() {
        eprintln!(
            "[biffix] inheriting params from {prior_id}: {}",
            names(&mut inherited.params.keys())
        );
    }
    if !inherited.skipped_secrets.is_empty() {
        eprintln!(
            "[biffix] not inherited (secret; pass again with --param/--params): {}",
            names(&mut inherited.skipped_secrets.iter())
        );
    }
    if !inherited.skipped_undeclared.is_empty() {
        eprintln!(
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
                println!("no runs");
            }
            for r in &runs {
                let progress = match (r.completed, r.total) {
                    (Some(c), Some(t)) => format!(" {c}/{t}"),
                    _ => String::new(),
                };
                println!("{}  {}{progress}", r.run_id, r.state);
            }
        }
    }
    Ok(())
}

pub(crate) async fn show(session: &mut Session, run_id: &str) -> CliResult<()> {
    let client = session.authed_client().await?;
    let detail = client.get_run(run_id).await?;
    render_detail(&detail, session.output);
    Ok(())
}

pub async fn cancel(session: &mut Session, run_id: &str) -> CliResult<()> {
    let client = session.authed_client().await?;
    client.cancel_run(run_id).await?;
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "run_id": run_id, "status": "cancel_requested",
        })),
        OutputMode::Human => println!("cancel requested for {run_id}"),
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
            Some(id) => println!("run {run_id} is in experiment {id}"),
            None => println!("run {run_id} is in no experiment"),
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
            Some(name) => println!("run {run_id} is named {name}"),
            None => println!(
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

fn render_detail(detail: &RunDetail, mode: OutputMode) {
    let s = &detail.summary;
    match mode {
        // The full `GET /runs/{id}` detail (params already secret-redacted server-side), not a
        // hand-picked subset that silently drops fields as the API grows.
        OutputMode::Json => crate::render::print_json(&serde_json::json!(detail)),
        OutputMode::Human => {
            println!("{}  {}", s.run_id, s.state);
            for t in &detail.tasks {
                let attempts = if t.attempts > 0 {
                    format!(" (attempt {})", t.attempts)
                } else {
                    String::new()
                };
                println!("  {}  {}{attempts}", t.task_key.as_str(), t.state);
            }
        }
    }
}

/// Poll until the run reaches a terminal state, then map that to the exit code (0/8/9). Transient
/// errors are retried within a budget and never reported as a run failure.
async fn follow_to_terminal(client: &ApiClient, run_id: &str, mode: OutputMode) -> CliResult<()> {
    let mut transient = 0u32;
    let mut last_state = String::new();
    loop {
        match client.get_run(run_id).await {
            Ok(detail) => {
                transient = 0;
                if detail.summary.state != last_state {
                    if mode == OutputMode::Human {
                        eprintln!("[biffix] {} → {}", run_id, detail.summary.state);
                    }
                    last_state = detail.summary.state.clone();
                }
                if let Some(outcome) =
                    follow_outcome(run_id, &detail.summary.state, detail.summary.terminal)
                {
                    render_detail(&detail, mode);
                    return outcome;
                }
            }
            Err(e) => {
                if let Some(stop) = poll_failed(run_id, e, &mut transient) {
                    return Err(stop);
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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
            eprintln!("[biffix] {run_id}: {e}; retrying");
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
}
