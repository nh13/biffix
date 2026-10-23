//! Why a run failed, in plain words: a failed step's attempts and the end of its
//! log become one cause, a sentence saying what happened, and what to try. Pure, so it is tested
//! with real log snippets; the API gathers the attempts and logs.

use crate::event::TerminationCategory;
use crate::translate::{category_from_reason, never_started, reason_words};

/// What made a step fail. The API carries it as the string [`FailureCause::as_str`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCause {
    /// Its container never started (most often: its image could not be pulled).
    CouldNotStart,
    /// It ran out of memory (an out-of-memory kill, exit 137 with no other cause, or an
    /// out-of-memory error in its log).
    OutOfMemory,
    /// AWS reclaimed the spot machine it ran on, more times than biffix retries.
    SpotReclaimed,
    /// Its machine went away for another reason.
    HostLost,
    /// It ran past its time limit (AWS Batch's attempt timeout) and was stopped.
    TimedOut,
    /// A file it needed was not there.
    MissingInput,
    /// An input file looks damaged (truncated, corrupt) or is not in the format the step expects.
    BadInput,
    /// The reference genome (or an index built from one) does not match the samples or the
    /// pipeline.
    ReferenceMismatch,
    /// It ran out of disk space.
    NoSpace,
    /// It was not allowed to read or write a file.
    AccessDenied,
    /// It finished without writing the files it declares.
    MissingOutput,
    /// It exited with an error.
    StepError,
    /// The workflow engine itself stopped with an error (no step failed).
    EngineError,
    /// biffix could not launch the run (preparing its images, or submitting its coordinator).
    LaunchFailed,
    /// Nothing recorded says why.
    Unknown,
}

impl FailureCause {
    /// Every cause, in declaration order (pinned to the variants by a test).
    pub const ALL: [FailureCause; 15] = [
        Self::CouldNotStart,
        Self::OutOfMemory,
        Self::SpotReclaimed,
        Self::HostLost,
        Self::TimedOut,
        Self::MissingInput,
        Self::BadInput,
        Self::ReferenceMismatch,
        Self::NoSpace,
        Self::AccessDenied,
        Self::MissingOutput,
        Self::StepError,
        Self::EngineError,
        Self::LaunchFailed,
        Self::Unknown,
    ];

    /// The wire name (`FailureItem.cause` in the API).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CouldNotStart => "could_not_start",
            Self::OutOfMemory => "out_of_memory",
            Self::SpotReclaimed => "spot_reclaimed",
            Self::HostLost => "host_lost",
            Self::TimedOut => "timed_out",
            Self::MissingInput => "missing_input",
            Self::BadInput => "bad_input",
            Self::ReferenceMismatch => "reference_mismatch",
            Self::NoSpace => "no_space",
            Self::AccessDenied => "access_denied",
            Self::MissingOutput => "missing_output",
            Self::StepError => "step_error",
            Self::EngineError => "engine_error",
            Self::LaunchFailed => "launch_failed",
            Self::Unknown => "unknown",
        }
    }

    /// What *Try again* does for this cause, before the run's own facts (a run that cannot be
    /// resumed is re-run instead). A cause the inputs or options brought on would fail the same
    /// way again unchanged, so those open the launch form to fix them first.
    pub fn try_again(self) -> TryAgain {
        match self {
            Self::MissingInput | Self::BadInput | Self::AccessDenied => TryAgain::FixSamples,
            Self::ReferenceMismatch | Self::EngineError => TryAgain::FixOptions,
            Self::LaunchFailed => TryAgain::Rerun,
            Self::CouldNotStart
            | Self::OutOfMemory
            | Self::SpotReclaimed
            | Self::HostLost
            | Self::TimedOut
            | Self::NoSpace
            | Self::MissingOutput
            | Self::StepError
            | Self::Unknown => TryAgain::Resume,
        }
    }
}

/// What *Try again* does for a failed run. The API carries it as the string
/// [`TryAgain::as_str`] returns (`FailureItem.try_again`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryAgain {
    /// Pick up where it stopped: finished steps are reused.
    Resume,
    /// Start again with the same inputs and options.
    Rerun,
    /// Fix the samples (their files or paths) first: open the launch form at the samples step.
    FixSamples,
    /// Fix the options (the reference, a setting) first: open the launch form at the options step.
    FixOptions,
}

impl TryAgain {
    /// Every mode, in declaration order.
    pub const ALL: [TryAgain; 4] = [Self::Resume, Self::Rerun, Self::FixSamples, Self::FixOptions];

    /// The wire name (`FailureItem.try_again` in the API).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Rerun => "rerun",
            Self::FixSamples => "fix_samples",
            Self::FixOptions => "fix_options",
        }
    }
}

/// One attempt of a failed step, as recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FailedAttempt {
    pub exit_code: Option<i64>,
    /// `oom`, `spot`, `host_terminated`, `app_error`, `missing_output`, or another recorded value.
    pub termination_category: Option<String>,
    /// AWS Batch's reason for the attempt's end.
    pub status_reason: Option<String>,
    /// EC2 warned the attempt's spot host would be reclaimed while it ran.
    pub spot_warned: bool,
}

/// A failed step's cause, what happened, and what to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnosis {
    pub cause: FailureCause,
    /// Completes "`<step>` failed: …", e.g. "it ran out of memory".
    pub message: String,
    pub advice: String,
}

/// A log pattern: lowercase fragments that must all appear in one line. A fragment starting with
/// `!` must NOT appear in that line instead (so `not in reference` can skip `reference panel`).
type LogPattern = &'static [&'static str];

const OOM_LOG: &[LogPattern] = &[
    &["outofmemoryerror"],
    &["out of memory"],
    &["cannot allocate memory"],
    &["std::bad_alloc"],
    &["memoryerror"],
    &["oom-kill"],
];
const MISSING_INPUT_LOG: &[LogPattern] = &[
    &["nosuchkey"],
    &["no such file or directory"],
    &["does not exist"],
    &["filenotfounderror"],
    &["filenotfoundexception"],
    &["the specified key does not exist"],
    // Snakemake, at DAG build: `MissingInputException in rule x …` / `Missing input files for rule
    // x:` (the `affected files:` it lists name no other marker).
    &["missinginputexception"],
    &["missing input files for rule"],
];

// A word too common alone (`truncated`, `malformed`, `unexpected end of file`, `403`) only counts
// beside the words that make it mean a damaged file, a foreign contig or a refused request: a
// shell's `syntax error: unexpected end of file` or Python's `malformed node or string` is a bug in
// the pipeline, not a damaged input.

/// A damaged input, or one not in the format the step expects (gzip and friends, htslib/bgzf,
/// htsjdk, FASTQ readers).
const BAD_INPUT_LOG: &[LogPattern] = &[
    &["not in gzip format"],
    &["gzip:", "unexpected end of file"],
    &["gunzip:", "unexpected end of file"],
    &["zcat:", "unexpected end of file"],
    &["pigz:", "unexpected end of file"],
    &["bgzip:", "unexpected end of file"],
    &["invalid compressed data"],
    &["crc error"],
    &["crc", "mismatch"],
    &["crc check failed"],
    &["probably truncated"],
    &["truncated file"],
    &["file is truncated"],
    &["truncated", "gzip"],
    &["truncated", "bgzf"],
    &["[e::bgzf"],
    &["invalid fastq"],
    &["not a valid fastq"],
    &["samformatexception"],
    &["malformed", "fastq"],
    &["malformed", "bam"],
    &["malformed", "vcf"],
    &["malformed", "record"],
];
/// A reference that does not match the reads, the variants or the pipeline's index (GATK/Picard
/// sequence dictionaries, samtools/bcftools faidx lookups, STAR's genome version check). An
/// imputation `reference panel` is not the reference genome.
const REFERENCE_MISMATCH_LOG: &[LogPattern] = &[
    &["not found in reference", "!reference panel"],
    &["not found in the reference", "!reference panel"],
    &["not in the reference", "!reference panel"],
    &["not in reference", "!reference panel"],
    &["incompatible contigs"],
    &["sequence dictionar", "differ"],
    &["sequence dictionar", "not the same"],
    &["sequence dictionar", "incompatible"],
    &["sequence dictionar", "not compatible"],
    &["not present in the sequence dictionary"],
    &["not in the sequence dictionary"],
    &["not present in the reference", "!reference panel"],
    &["genome version", "incompatible"],
];
const NO_SPACE_LOG: &[LogPattern] = &[&["no space left on device"], &["disk quota exceeded"]];
/// A refused read or write (S3's `AccessDenied` / 403, IAM's "is not authorized").
const ACCESS_DENIED_LOG: &[LogPattern] = &[
    &["accessdenied"],
    &["access denied"],
    &["403 forbidden"],
    &["(403)", "forbidden"],
    &["not authorized to perform"],
];

/// Whether one line of the log matches one of `patterns` (see [`LogPattern`]).
fn log_matches(log_tail: &[String], patterns: &[LogPattern]) -> bool {
    log_tail.iter().any(|line| {
        let lower = line.to_lowercase();
        patterns.iter().any(|all| {
            all.iter().all(|fragment| match fragment.strip_prefix('!') {
                Some(absent) => !lower.contains(absent),
                None => lower.contains(fragment),
            })
        })
    })
}

/// The causes read from a step's log after a missing input and before a plain step error, in the
/// order they are tried: `(patterns, cause, message, advice)`. An explicit full disk comes first,
/// since the truncated files it leaves behind would otherwise read as a damaged input.
const LOG_CAUSES: &[(&[LogPattern], FailureCause, &str, &str)] = &[
    (
        NO_SPACE_LOG,
        FailureCause::NoSpace,
        "it ran out of disk space",
        "This is not a problem with your inputs. Ask the workflow's author for more scratch space \
         for this step.",
    ),
    (
        BAD_INPUT_LOG,
        FailureCause::BadInput,
        "one of its input files looks damaged or is not in the format it expects",
        "Download or upload this sample's files again; the log below names the file.",
    ),
    (
        REFERENCE_MISMATCH_LOG,
        FailureCause::ReferenceMismatch,
        "the reference genome does not match the samples (or the pipeline's index)",
        "Pick the reference that matches the samples' species and genome build.",
    ),
    (
        ACCESS_DENIED_LOG,
        FailureCause::AccessDenied,
        "it was not allowed to read or write a file",
        "Ask an admin: the analysis cannot reach this location. The log below names it.",
    ),
];

/// The first of [`LOG_CAUSES`] the log shows.
fn input_and_environment_causes(log_tail: &[String]) -> Option<Diagnosis> {
    LOG_CAUSES.iter().find(|(patterns, ..)| log_matches(log_tail, patterns)).map(
        |&(_, cause, message, advice)| Diagnosis {
            cause,
            message: message.into(),
            advice: advice.into(),
        },
    )
}

/// The attempt's category: the one recorded with it, or, when none was, the one the translator
/// derives from its status reason (a defensive re-read with the same whole-word rules, so the two
/// never disagree).
fn category(a: &FailedAttempt) -> Option<TerminationCategory> {
    match a.termination_category.as_deref().map(str::trim) {
        Some(c) if !c.is_empty() => Some(TerminationCategory::parse(c)),
        _ => category_from_reason(a.status_reason.as_deref()),
    }
}

/// Why a step failed, from its attempts (in order) and the last lines of its log. The last attempt
/// decides, except that repeated spot reclaims are counted over all of them.
///
/// Recorded facts outrank guesses: a container that never started, then the attempt's category
/// (out of memory, spot reclaim, lost host, missing output), then a recorded timeout, then the
/// heuristics (exit 137, which is any SIGKILL; memory errors or missing files named in the log;
/// then a full disk, a damaged input, a mismatched reference or a refused access named in the
/// log), then a plain step error.
pub fn diagnose(attempts: &[FailedAttempt], log_tail: &[String]) -> Diagnosis {
    let last = attempts.last().cloned().unwrap_or_default();
    let reason = last.status_reason.as_deref().unwrap_or("").to_lowercase();
    let words = reason_words(&reason);
    let reclaims = attempts
        .iter()
        .filter(|a| a.spot_warned || category(a) == Some(TerminationCategory::Spot))
        .count();
    let tries = attempts.len().max(1);
    // A negative exit code is the translator's "none was reported" sentinel (`-1`), not a real
    // exit: a container exits 0..=255.
    let exit_code = last.exit_code.filter(|c| *c >= 0);

    // Batch could not start its container: nothing ran, so there is no log and no real exit code.
    // Any `Cannot…ContainerError` counts, as it does for the translator.
    if words.contains(&"cannotpullcontainererror") {
        return Diagnosis {
            cause: FailureCause::CouldNotStart,
            message: "its software image could not be downloaded, so it never started".into(),
            advice: "This is not a problem with your inputs. The image this workflow version uses \
                     may have been removed or may not be readable; register the workflow again (or \
                     ask its author to), then launch it again."
                .into(),
        };
    }
    if reason.contains("task failed to start") || never_started(&words) {
        return Diagnosis {
            cause: FailureCause::CouldNotStart,
            message: "its container could not be started, so it never ran".into(),
            advice: "This is not a problem with your inputs. Launch it again; if it keeps \
                     happening, tell the workflow's author, quoting the reason below."
                .into(),
        };
    }
    let out_of_memory = Diagnosis {
        cause: FailureCause::OutOfMemory,
        message: "it ran out of memory".into(),
        advice: "Give this step more memory (its resources in the workflow), or split its input \
                 into smaller pieces."
            .into(),
    };
    let last_category = category(&last);
    if last_category == Some(TerminationCategory::Oom) {
        return out_of_memory;
    }
    if last.spot_warned || last_category == Some(TerminationCategory::Spot) {
        return Diagnosis {
            cause: FailureCause::SpotReclaimed,
            message: format!(
                "AWS took back the spot machine it ran on ({reclaims} of {tries} {})",
                if tries == 1 { "try" } else { "tries" }
            ),
            advice: "This is not a problem with your inputs. Resume the run to try the step \
                     again; if it keeps happening, run the step on on-demand machines."
                .into(),
        };
    }
    if last_category == Some(TerminationCategory::HostTerminated) {
        return Diagnosis {
            cause: FailureCause::HostLost,
            message: "the machine it ran on stopped unexpectedly".into(),
            advice: "This is not a problem with your inputs. Resume the run to try the step again."
                .into(),
        };
    }
    if last_category == Some(TerminationCategory::MissingOutput) {
        return Diagnosis {
            cause: FailureCause::MissingOutput,
            message: "it finished without writing the files it should have".into(),
            advice: "This is usually a problem in the workflow itself; tell its author, with the \
                     log below."
                .into(),
        };
    }
    // Batch stops an attempt that runs past its timeout with SIGTERM and then SIGKILL, so it exits
    // 137 like an out-of-memory kill; the reason tells the two apart. Batch says `duration exceeded
    // timeout`; a bare `timeout` word is more often a network one quoted from a container that
    // could not start its log driver or reach a registry.
    let timed_out = reason.contains("duration exceeded timeout") || reason.contains("timed out");
    if timed_out {
        return Diagnosis {
            cause: FailureCause::TimedOut,
            message: match exit_code {
                Some(code) if code != 0 => {
                    format!("it ran past its time limit and was stopped (exit code {code})")
                }
                _ => "it ran past its time limit and was stopped".into(),
            },
            advice: "Ask the pipeline's author for more time for this step.".into(),
        };
    }
    if exit_code == Some(137) || log_matches(log_tail, OOM_LOG) {
        return out_of_memory;
    }
    if log_matches(log_tail, MISSING_INPUT_LOG) {
        return missing_input();
    }
    if let Some(d) = input_and_environment_causes(log_tail) {
        return d;
    }
    if last_category == Some(TerminationCategory::AppError) || exit_code.is_some_and(|c| c != 0) {
        return Diagnosis {
            cause: FailureCause::StepError,
            message: match exit_code {
                Some(code) if code != 0 => {
                    format!("the step exited with an error (exit code {code})")
                }
                _ => "the step exited with an error".into(),
            },
            advice: "The last lines of its log, below, usually say why.".into(),
        };
    }
    Diagnosis {
        cause: FailureCause::Unknown,
        message: "it failed, and biffix recorded no reason".into(),
        advice: "Open the step's log to see what happened.".into(),
    }
}

/// Lines that open the epilogue a Snakemake job's own Snakemake writes after its rule fails: with
/// `--verbose` (which the AWS Batch executor passes) a `Full Traceback` of the rule's exception,
/// else the `RuleException:` block, then `Error in rule …:`, `Shutting down…`, and the
/// `WorkflowError` traceback that ends every failed job's log.
const SNAKEMAKE_JOB_EPILOGUE: &[&str] = &[
    "Full Traceback (most recent call last):",
    "RuleException:",
    "Error in rule ",
    "Shutting down, this might take some time.",
];

/// What a failed Snakemake job's log says about its step ([`snakemake_job_log`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnakemakeJobLog {
    /// The last lines the step itself wrote, above its Snakemake's failure epilogue.
    pub step_lines: Vec<String>,
    /// The epilogue's line(s) naming the step's shell command's failure (Python's
    /// `CalledProcessError: Command '…' returned non-zero exit status 3.`); empty when it has none.
    pub error_lines: Vec<String>,
    /// The step's shell command's own exit code, read from that error. The job's container exits 1
    /// whatever its command did (its Snakemake failed), so this is the only record of it.
    pub step_exit_code: Option<i64>,
}

impl SnakemakeJobLog {
    /// The tail to show: the step's last lines, then its error, `n` lines at most.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let error = &self.error_lines[..self.error_lines.len().min(n)];
        let own = &self.step_lines[self.step_lines.len().saturating_sub(n - error.len())..];
        own.iter().chain(error).cloned().collect()
    }
}

/// How Python's `CalledProcessError` ends its message for a command that exited non-zero.
const EXIT_STATUS: &str = "returned non-zero exit status ";
/// … and for one a signal killed (`died with <Signals.SIGKILL: 9>.`).
const DIED_WITH_SIGNAL: &str = "died with <Signals.";

/// The exit code a `CalledProcessError` line names: its exit status, or 128 + the signal that
/// killed the command (the shell's convention, so a SIGKILL reads as 137 either way).
fn called_process_exit(line: &str) -> Option<i64> {
    if let Some(at) = line.find(EXIT_STATUS) {
        let rest = &line[at + EXIT_STATUS.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        return digits.parse().ok();
    }
    let at = line.find(DIED_WITH_SIGNAL)?;
    let rest = &line[at + DIED_WITH_SIGNAL.len()..];
    let number = rest.split_once(": ")?.1;
    let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().ok().map(|signal| 128 + signal)
}

/// The step's own lines (up to `n`) and its command's failure, from the end of a Snakemake job's
/// log (`lines`, oldest first). A Snakemake job on AWS Batch runs the rule under its own Snakemake,
/// whose failure epilogue (the rule's traceback, `Error in rule`, `WorkflowError: At least one job
/// did not complete successfully.`) is always the log's last ~40 lines, so a plain tail shows only
/// the engine's lines, never the step's (for an out-of-memory kill, the shell's `Killed`). This cuts
/// the log at the first epilogue line and keeps the `n` before it. A log with no epilogue (the job
/// was killed before Snakemake could write one), or nothing before it, keeps its last `n` lines.
///
/// The epilogue's first `CalledProcessError` is kept apart: the line that opens it and the one
/// that names the exit status (one line unless the command spans several), and that exit status.
/// Apart, so the command's text it quotes is shown but never read as the step's own output.
pub fn snakemake_job_log(lines: &[String], n: usize) -> SnakemakeJobLog {
    let opens_epilogue =
        |line: &String| SNAKEMAKE_JOB_EPILOGUE.iter().any(|m| line.trim_start().starts_with(m));
    let epilogue_at = lines.iter().position(opens_epilogue);
    let own = match epilogue_at {
        Some(cut) if cut > 0 => &lines[..cut],
        _ => lines,
    };
    // A timestamp line (`[Thu Oct  1 00:32:36 2026]`) just before the cut is the epilogue's.
    let own = match own.split_last() {
        Some((last, rest)) if !rest.is_empty() && is_snakemake_timestamp(last) => rest,
        _ => own,
    };
    let mut log = SnakemakeJobLog {
        step_lines: own[own.len().saturating_sub(n)..].to_vec(),
        ..Default::default()
    };
    // With no epilogue there is no failure of Snakemake's to read: every line is the step's.
    let epilogue = epilogue_at.map_or(&[][..], |at| &lines[at..]);
    if let Some((end, code)) =
        epilogue.iter().enumerate().find_map(|(i, l)| called_process_exit(l).map(|c| (i, c)))
    {
        let start =
            epilogue[..=end].iter().rposition(|l| l.contains("CalledProcessError")).unwrap_or(end);
        log.error_lines.push(epilogue[start].clone());
        if start != end {
            log.error_lines.push(epilogue[end].clone());
        }
        log.step_exit_code = Some(code);
    }
    log
}

/// A Snakemake log timestamp line: `[` + a date + `]` alone on the line.
fn is_snakemake_timestamp(line: &str) -> bool {
    let line = line.trim();
    line.len() > 2
        && line.starts_with('[')
        && line.ends_with(']')
        && line.chars().any(|c| c.is_ascii_digit())
}

/// Why a run failed when no step did: the workflow engine stopped (a bad parameter, a workflow
/// error), judged from the end of its own log. With no log to read (the coordinator's log is
/// missing or unreadable) nothing says the options are at fault, so the cause is unknown.
pub fn diagnose_engine(log_tail: &[String]) -> Diagnosis {
    if log_tail.iter().all(|line| line.trim().is_empty()) {
        return Diagnosis {
            cause: FailureCause::Unknown,
            message: "the workflow engine stopped, and biffix could not read its log".into(),
            advice: "Open the coordinator's log in Expert view to see what happened.".into(),
        };
    }
    if log_matches(log_tail, MISSING_INPUT_LOG) {
        return missing_input();
    }
    // The engine's own process (the Nextflow head JVM, a Snakemake DAG build over a large
    // scatter): no option the user can change fixes it.
    if log_matches(log_tail, OOM_LOG) {
        return engine_out_of_memory();
    }
    if let Some(d) = input_and_environment_causes(log_tail) {
        return d;
    }
    Diagnosis {
        cause: FailureCause::EngineError,
        message: "the workflow engine stopped with an error before any step failed".into(),
        advice: "The engine's log, below, says why: often a parameter or input it could not use."
            .into(),
    }
}

/// Why a run failed when no step did, from Batch's account of the coordinator job's stop (`stop`:
/// its exit code, reason and category) and the end of the engine's log. Recorded facts outrank the
/// log, as in [`diagnose`]: a coordinator whose container never started (its image could not be
/// pulled) has no log at all, and one Batch stopped for memory or lost with its machine says so in
/// its reason. Anything else is read from the log by [`diagnose_engine`].
pub fn diagnose_coordinator(stop: &FailedAttempt, log_tail: &[String]) -> Diagnosis {
    let reason = stop.status_reason.as_deref().unwrap_or("").to_lowercase();
    let words = reason_words(&reason);
    if words.contains(&"cannotpullcontainererror") {
        return Diagnosis {
            cause: FailureCause::CouldNotStart,
            message: "the workflow engine's software image could not be downloaded, so it never \
                      started"
                .into(),
            advice: "This is not a problem with your inputs. The engine image this workflow \
                     version uses may have been removed or may not be readable; register the \
                     workflow again (or ask its author to), then launch it again."
                .into(),
        };
    }
    if reason.contains("task failed to start") || never_started(&words) {
        return Diagnosis {
            cause: FailureCause::CouldNotStart,
            message: "the workflow engine's container could not be started, so it never ran".into(),
            advice: "This is not a problem with your inputs. Launch it again; if it keeps \
                     happening, ask an admin, quoting the reason below."
                .into(),
        };
    }
    match category(stop) {
        Some(TerminationCategory::Oom) => engine_out_of_memory(),
        Some(TerminationCategory::Spot) => Diagnosis {
            cause: FailureCause::SpotReclaimed,
            message: "AWS took back the spot machine the workflow engine ran on".into(),
            advice: "This is not a problem with your inputs. Resume the run to pick up where it \
                     stopped."
                .into(),
        },
        Some(TerminationCategory::HostTerminated) => Diagnosis {
            cause: FailureCause::HostLost,
            message: "the machine the workflow engine ran on stopped unexpectedly".into(),
            advice: "This is not a problem with your inputs. Resume the run to pick up where it \
                     stopped."
                .into(),
        },
        _ => diagnose_engine(log_tail),
    }
}

/// A file the work needed was not found (named in its log).
fn missing_input() -> Diagnosis {
    Diagnosis {
        cause: FailureCause::MissingInput,
        message: "a file it needed was not found".into(),
        advice: "Check the input paths you gave; the log below names the missing file.".into(),
    }
}

/// The workflow engine itself ran out of memory (its own process, not a step's).
fn engine_out_of_memory() -> Diagnosis {
    Diagnosis {
        cause: FailureCause::OutOfMemory,
        message: "the workflow engine ran out of memory".into(),
        advice: "This is not a problem with one step. Ask an admin for a larger coordinator, or \
                 split the run's samples over several runs."
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// The end of a real Snakemake job log on AWS Batch whose step was OOM-killed (abridged).
    const OOM_JOB_LOG: &str = "\
Execute 1 jobs...
[Thu Oct  1 00:32:35 2026]
localrule align:
    wildcards: sample=B
Waiting for running jobs to complete.
/usr/bin/bash: line 1:    15 Killed                  python3 -c 'x = bytearray(4 * 1024**3)'
Full Traceback (most recent call last):
  File \"/opt/sm/snakemake/executors/local.py\", line 432, in run_wrapper
subprocess.CalledProcessError: Command 'python3' returned non-zero exit status 137.
RuleException:
CalledProcessError in file \"/work/src/Snakefile\", line 36:
[Thu Oct  1 00:32:36 2026]
Error in rule align:
    jobid: 0
Shutting down, this might take some time.
Exiting because a job execution failed. Look above for error messages
WorkflowError:
At least one job did not complete successfully.";

    #[test]
    fn a_snakemake_job_tail_is_the_step_s_own_lines_not_the_engine_s() {
        let log = snakemake_job_log(&lines(OOM_JOB_LOG), 3);
        assert_eq!(
            log.step_lines,
            [
                "    wildcards: sample=B",
                "Waiting for running jobs to complete.",
                "/usr/bin/bash: line 1:    15 Killed                  python3 -c 'x = bytearray(4 * 1024**3)'",
            ]
        );
        // The tail ends with the command's failure, in the room the step's first lines leave.
        assert_eq!(
            log.tail(3),
            [
                "Waiting for running jobs to complete.",
                "/usr/bin/bash: line 1:    15 Killed                  python3 -c 'x = bytearray(4 * 1024**3)'",
                "subprocess.CalledProcessError: Command 'python3' returned non-zero exit status 137.",
            ]
        );
        assert_eq!(log.step_exit_code, Some(137));
        // Without --verbose the epilogue opens at the RuleException block, after a timestamp.
        let quiet = lines(
            "step output\nlast step line\n[Thu Oct  1 00:32:36 2026]\nRuleException:\nWorkflowError:",
        );
        assert_eq!(snakemake_job_log(&quiet, 20).tail(20), ["step output", "last step line"]);
    }

    #[test]
    fn a_snakemake_job_log_with_no_epilogue_keeps_its_tail() {
        // Killed before Snakemake could write anything more, or nothing but the epilogue.
        let killed = lines("a\nb\nc");
        assert_eq!(snakemake_job_log(&killed, 2).tail(2), ["b", "c"]);
        let only = lines("Error in rule x:\nWorkflowError:");
        assert_eq!(snakemake_job_log(&only, 20).tail(20), only);
        assert_eq!(snakemake_job_log(&[], 20), SnakemakeJobLog::default());
    }

    /// The end of a Snakemake job's log on AWS Batch whose shell command exited 3 (abridged): the
    /// job's container exits 1, so the log is the only record of the 3.
    const EXIT_3_JOB_LOG: &str = "\
localrule world:
    jobid: 0
smoke: failing on purpose
Full Traceback (most recent call last):
  File \"/opt/biffix/sm/lib/python3.12/site-packages/snakemake/shell.py\", line 381, in __new__
    raise sp.CalledProcessError(retcode, cmd)
subprocess.CalledProcessError: Command 'set -euo pipefail;  if [ 3 != 0 ]; then exit 3; fi' returned non-zero exit status 3.
RuleException:
CalledProcessError in file \"/work/src/Snakefile\", line 12:
Command 'set -euo pipefail;  if [ 3 != 0 ]; then exit 3; fi' returned non-zero exit status 3.
[Fri Oct 23 13:00:50 2026]
Error in rule world:
    jobid: 0
Exiting because a job execution failed. Look above for error messages
WorkflowError:
At least one job did not complete successfully.";

    #[test]
    fn a_snakemake_job_log_names_its_command_s_own_exit_code_and_error() {
        let log = snakemake_job_log(&lines(EXIT_3_JOB_LOG), 20);
        assert_eq!(log.step_exit_code, Some(3));
        assert_eq!(
            log.step_lines,
            ["localrule world:", "    jobid: 0", "smoke: failing on purpose"]
        );
        assert_eq!(
            log.error_lines,
            ["subprocess.CalledProcessError: Command 'set -euo pipefail;  if [ 3 != 0 ]; then \
                 exit 3; fi' returned non-zero exit status 3."]
        );
        assert_eq!(log.tail(20).last(), log.error_lines.last());
        // A tail too short for both keeps the error.
        assert_eq!(log.tail(1), log.error_lines);

        // Without --verbose the RuleException block names it.
        let quiet = lines(
            "out\n[Fri Oct 23 13:00:50 2026]\nRuleException:\n\
             CalledProcessError in file \"/work/src/Snakefile\", line 12:\n\
             Command 'exit 4' returned non-zero exit status 4.\nWorkflowError:",
        );
        let log = snakemake_job_log(&quiet, 20);
        assert_eq!(log.step_exit_code, Some(4));
        assert_eq!(
            log.tail(20),
            [
                "out",
                "CalledProcessError in file \"/work/src/Snakefile\", line 12:",
                "Command 'exit 4' returned non-zero exit status 4.",
            ]
        );
    }

    #[test]
    fn a_multi_line_command_s_error_keeps_its_first_and_last_lines() {
        let log = snakemake_job_log(
            &log(&[
                "out",
                "Full Traceback (most recent call last):",
                "subprocess.CalledProcessError: Command 'set -euo pipefail;  samtools sort x \\",
                "    -o y \\",
                "    -@ 4' returned non-zero exit status 2.",
                "WorkflowError:",
            ]),
            20,
        );
        assert_eq!(log.step_exit_code, Some(2));
        assert_eq!(
            log.error_lines,
            [
                "subprocess.CalledProcessError: Command 'set -euo pipefail;  samtools sort x \\",
                "    -@ 4' returned non-zero exit status 2.",
            ]
        );
    }

    #[test]
    fn a_command_a_signal_killed_reads_as_128_plus_the_signal() {
        let log = snakemake_job_log(
            &log(&[
                "out",
                "Full Traceback (most recent call last):",
                "subprocess.CalledProcessError: Command 'x' died with <Signals.SIGKILL: 9>.",
            ]),
            20,
        );
        assert_eq!(log.step_exit_code, Some(137));
    }

    #[test]
    fn a_log_without_an_epilogue_names_no_command_failure() {
        // The step's own output (a script that printed a traceback) is not its Snakemake's.
        let log1 = snakemake_job_log(
            &log(&["subprocess.CalledProcessError: Command 'x' returned non-zero exit status 5."]),
            20,
        );
        assert_eq!(log1.step_exit_code, None);
        assert!(log1.error_lines.is_empty());
        // Nor does an epilogue without one (a `run:` block's Python error).
        let log2 = snakemake_job_log(
            &log(&["out", "RuleException:", "KeyError: 'x'", "WorkflowError:"]),
            20,
        );
        assert_eq!(log2.step_exit_code, None);
        assert_eq!(log2.tail(20), ["out"]);
    }

    fn attempt(category: Option<&str>, exit: Option<i64>) -> FailedAttempt {
        FailedAttempt {
            exit_code: exit,
            termination_category: category.map(str::to_string),
            ..Default::default()
        }
    }
    fn log(lines: &[&str]) -> Vec<String> {
        lines.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn all_is_every_cause_once() {
        // No wildcard: a new cause fails to compile here until it has a position in `ALL`.
        let position = |c: FailureCause| match c {
            FailureCause::CouldNotStart => 0,
            FailureCause::OutOfMemory => 1,
            FailureCause::SpotReclaimed => 2,
            FailureCause::HostLost => 3,
            FailureCause::TimedOut => 4,
            FailureCause::MissingInput => 5,
            FailureCause::BadInput => 6,
            FailureCause::ReferenceMismatch => 7,
            FailureCause::NoSpace => 8,
            FailureCause::AccessDenied => 9,
            FailureCause::MissingOutput => 10,
            FailureCause::StepError => 11,
            FailureCause::EngineError => 12,
            FailureCause::LaunchFailed => 13,
            FailureCause::Unknown => 14,
        };
        for (i, cause) in FailureCause::ALL.into_iter().enumerate() {
            assert_eq!(position(cause), i, "{cause:?}");
        }
        let names: std::collections::BTreeSet<&str> =
            FailureCause::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names.len(), FailureCause::ALL.len(), "wire names are distinct");
    }

    #[test]
    fn a_container_that_never_started_says_so_not_step_error() {
        let pull = FailedAttempt {
            exit_code: Some(-1),
            status_reason: Some(
                "Task failed to start: CannotPullContainerError: manifest for repo@sha256:0f34 \
                 not found: manifest unknown"
                    .into(),
            ),
            ..Default::default()
        };
        let d = diagnose(&[pull], &[]);
        assert_eq!(d.cause, FailureCause::CouldNotStart);
        assert!(d.message.contains("image could not be downloaded"), "{}", d.message);
        let other = FailedAttempt {
            exit_code: Some(-1),
            status_reason: Some("Task failed to start".into()),
            ..Default::default()
        };
        assert_eq!(diagnose(&[other], &[]).cause, FailureCause::CouldNotStart);
    }

    #[test]
    fn out_of_memory_from_category_exit_137_or_the_log() {
        for (attempts, tail) in [
            (vec![attempt(Some("oom"), None)], vec![]),
            (vec![attempt(Some("app_error"), Some(137))], vec![]),
            (
                vec![attempt(Some("app_error"), Some(1))],
                log(&["Exception in thread \"main\" java.lang.OutOfMemoryError: Java heap space"]),
            ),
            (
                vec![attempt(Some("app_error"), Some(1))],
                log(&["[E::bwa_idx_load] fail to allocate: Cannot allocate memory"]),
            ),
        ] {
            let d = diagnose(&attempts, &tail);
            assert_eq!(d.cause, FailureCause::OutOfMemory, "{attempts:?} {tail:?}");
            assert_eq!(d.message, "it ran out of memory");
        }
    }

    #[test]
    fn spot_reclaims_are_counted_over_every_try() {
        let reclaimed = FailedAttempt {
            spot_warned: true,
            status_reason: Some("Host EC2 (instance i-0abc) terminated.".into()),
            ..Default::default()
        };
        let d = diagnose(&[reclaimed.clone(), reclaimed.clone(), reclaimed.clone()], &[]);
        assert_eq!(d.cause, FailureCause::SpotReclaimed);
        assert_eq!(d.message, "AWS took back the spot machine it ran on (3 of 3 tries)");
        // Reclaims are counted, not tries: a middle try that failed on its own is not one.
        let d = diagnose(
            &[reclaimed.clone(), attempt(Some("app_error"), Some(1)), attempt(Some("spot"), None)],
            &[],
        );
        assert_eq!(d.message, "AWS took back the spot machine it ran on (2 of 3 tries)");
        assert_eq!(
            diagnose(&[reclaimed], &[]).message,
            "AWS took back the spot machine it ran on (1 of 1 try)"
        );
    }

    #[test]
    fn a_recorded_category_outranks_exit_137_and_the_log() {
        // A spot drain or a lost host can SIGKILL the container (exit 137): not out of memory.
        let spot_killed = FailedAttempt {
            exit_code: Some(137),
            termination_category: Some("spot".into()),
            status_reason: Some("Host EC2 (instance i-0abc) terminated.".into()),
            spot_warned: true,
        };
        assert_eq!(diagnose(&[spot_killed], &log(&["Killed"])).cause, FailureCause::SpotReclaimed);
        let warned_only =
            FailedAttempt { exit_code: Some(137), spot_warned: true, ..Default::default() };
        assert_eq!(diagnose(&[warned_only], &[]).cause, FailureCause::SpotReclaimed);
        assert_eq!(
            diagnose(&[attempt(Some("host_terminated"), Some(137))], &[]).cause,
            FailureCause::HostLost
        );
        // A missing output whose log happens to say a file does not exist is still a missing
        // output, not a missing input.
        let d = diagnose(
            &[attempt(Some("missing_output"), Some(1))],
            &log(&["mkdir: cannot stat 'out/x': No such file or directory"]),
        );
        assert_eq!(d.cause, FailureCause::MissingOutput);
        // A recorded category wins over the reason's words.
        let recorded = FailedAttempt {
            exit_code: Some(1),
            termination_category: Some("app_error".into()),
            status_reason: Some("Host EC2 (instance i-0abc) terminated.".into()),
            ..Default::default()
        };
        assert_eq!(diagnose(&[recorded], &[]).cause, FailureCause::StepError);
    }

    #[test]
    fn a_timeout_that_ended_in_sigkill_is_not_out_of_memory() {
        let timed_out = FailedAttempt {
            exit_code: Some(137),
            status_reason: Some("Job attempt duration exceeded timeout".into()),
            ..Default::default()
        };
        let d = diagnose(&[timed_out], &[]);
        assert_eq!(d.cause, FailureCause::TimedOut);
        assert_eq!(d.message, "it ran past its time limit and was stopped (exit code 137)");
        assert_eq!(d.advice, "Ask the pipeline's author for more time for this step.");
    }

    #[test]
    fn a_reason_without_a_category_is_read_with_whole_words() {
        // An attempt recorded without a category: its reason is read as the translator reads it.
        let oom = FailedAttempt {
            exit_code: Some(1),
            status_reason: Some("OutOfMemoryError: Container killed due to memory usage".into()),
            ..Default::default()
        };
        assert_eq!(diagnose(&[oom], &[]).cause, FailureCause::OutOfMemory);
        // `bloom` is not `oom`, and a `hotspot` is not a spot machine.
        let words = FailedAttempt {
            exit_code: Some(1),
            status_reason: Some("Essential container in task exited: bloom hotspot".into()),
            ..Default::default()
        };
        assert_eq!(diagnose(&[words], &[]).cause, FailureCause::StepError);
    }

    #[test]
    fn any_cannot_container_error_never_started() {
        for reason in [
            "CannotStartContainerError: Error response from daemon: failed to create shim",
            "CannotCreateContainerError: Error response from daemon: devmapper",
        ] {
            let a = FailedAttempt {
                exit_code: Some(-1),
                status_reason: Some(reason.into()),
                ..Default::default()
            };
            let d = diagnose(&[a], &[]);
            assert_eq!(d.cause, FailureCause::CouldNotStart, "{reason}");
            assert_eq!(d.message, "its container could not be started, so it never ran");
        }
    }

    #[test]
    fn every_cause_has_its_wire_name() {
        let all = [
            (FailureCause::CouldNotStart, "could_not_start"),
            (FailureCause::OutOfMemory, "out_of_memory"),
            (FailureCause::SpotReclaimed, "spot_reclaimed"),
            (FailureCause::HostLost, "host_lost"),
            (FailureCause::TimedOut, "timed_out"),
            (FailureCause::MissingInput, "missing_input"),
            (FailureCause::BadInput, "bad_input"),
            (FailureCause::ReferenceMismatch, "reference_mismatch"),
            (FailureCause::NoSpace, "no_space"),
            (FailureCause::AccessDenied, "access_denied"),
            (FailureCause::MissingOutput, "missing_output"),
            (FailureCause::StepError, "step_error"),
            (FailureCause::EngineError, "engine_error"),
            (FailureCause::LaunchFailed, "launch_failed"),
            (FailureCause::Unknown, "unknown"),
        ];
        for (cause, name) in all {
            assert_eq!(cause.as_str(), name);
        }
    }

    #[test]
    fn a_lost_host_that_was_not_a_spot_reclaim() {
        let lost = FailedAttempt {
            status_reason: Some("Host EC2 (instance i-0abc) terminated.".into()),
            ..Default::default()
        };
        assert_eq!(diagnose(&[lost], &[]).cause, FailureCause::HostLost);
        assert_eq!(
            diagnose(&[attempt(Some("host_terminated"), None)], &[]).cause,
            FailureCause::HostLost
        );
    }

    #[test]
    fn a_missing_input_is_read_from_the_log() {
        for line in [
            "fatal error: An error occurred (NoSuchKey) when calling the GetObject operation",
            "samtools: can't open \"sample1.bam\": No such file or directory",
            "ERROR ~ Path `s3://b/in/sample1.fastq.gz` does not exist",
            "FileNotFoundError: [Errno 2] No such file: 'x.csv'",
        ] {
            let d = diagnose(&[attempt(Some("app_error"), Some(1))], &log(&[line]));
            assert_eq!(d.cause, FailureCause::MissingInput, "{line}");
        }
    }

    #[test]
    fn missing_output_and_plain_errors() {
        assert_eq!(
            diagnose(&[attempt(Some("missing_output"), Some(0))], &[]).cause,
            FailureCause::MissingOutput
        );
        let d = diagnose(&[attempt(Some("app_error"), Some(1))], &log(&["failing on purpose"]));
        assert_eq!(d.cause, FailureCause::StepError);
        assert_eq!(d.message, "the step exited with an error (exit code 1)");
        assert_eq!(
            diagnose(&[attempt(Some("app_error"), None)], &[]).message,
            "the step exited with an error"
        );
        assert_eq!(diagnose(&[], &[]).cause, FailureCause::Unknown);
    }

    #[test]
    fn a_missing_exit_code_is_unknown_not_a_step_error() {
        // The translator records `-1` when AWS Batch reported no exit code.
        let unreported = FailedAttempt {
            exit_code: Some(-1),
            status_reason: Some("Terminated by user request".into()),
            ..Default::default()
        };
        assert_eq!(diagnose(&[unreported], &[]).cause, FailureCause::Unknown);
        let timed_out = FailedAttempt {
            exit_code: Some(-1),
            status_reason: Some("Job attempt duration exceeded timeout".into()),
            ..Default::default()
        };
        assert_eq!(
            diagnose(&[timed_out], &[]).message,
            "it ran past its time limit and was stopped"
        );
        assert_eq!(
            diagnose(&[attempt(Some("app_error"), Some(-1))], &[]).message,
            "the step exited with an error"
        );
    }

    #[test]
    fn the_last_attempt_decides() {
        // Reclaimed once, then a real error on the retry: the error is why it failed.
        let first = FailedAttempt { spot_warned: true, ..Default::default() };
        let d = diagnose(&[first, attempt(Some("app_error"), Some(2))], &[]);
        assert_eq!(d.cause, FailureCause::StepError);
    }

    #[test]
    fn an_engine_failure_without_a_failed_step() {
        assert_eq!(
            diagnose_engine(&log(&["ERROR ~ Unknown parameter: --genome"])).cause,
            FailureCause::EngineError
        );
        assert_eq!(
            diagnose_engine(&log(&["ERROR ~ Path `s3://b/sheet.csv` does not exist"])).cause,
            FailureCause::MissingInput
        );
        // Snakemake stops at DAG build with its own wording for a sample path that is not there.
        let d = diagnose_engine(&log(&[
            "Building DAG of jobs...",
            "MissingInputException in rule align in file /work/Snakefile, line 12:",
            "Missing input files for rule align:",
            "    output: results/A.bam",
            "    wildcards: sample=A",
            "    affected files:",
            "        s3://b/in/A.fastq.gz",
        ]));
        assert_eq!(d.cause, FailureCause::MissingInput);
        assert_eq!(d.cause.try_again(), TryAgain::FixSamples);
    }

    #[test]
    fn a_timeout_is_read_from_the_recorded_reason_alone() {
        for reason in ["Job attempt duration exceeded timeout", "Task timed out after 3600 seconds"]
        {
            let a = FailedAttempt {
                exit_code: None,
                status_reason: Some(reason.into()),
                ..Default::default()
            };
            let d = diagnose(&[a], &[]);
            assert_eq!(d.cause, FailureCause::TimedOut, "{reason}");
            assert_eq!(d.message, "it ran past its time limit and was stopped");
        }
        // A log line about a timeout is not a recorded timeout.
        let d = diagnose(
            &[attempt(Some("app_error"), Some(1))],
            &log(&["WARN: connection timeout, retrying"]),
        );
        assert_eq!(d.cause, FailureCause::StepError);
        // Nor is a network timeout quoted in the reason (a log driver that could not connect).
        let a = FailedAttempt {
            exit_code: Some(1),
            status_reason: Some(
                "ResourceInitializationError: failed to get log driver: dial tcp 10.0.0.1:443: \
                 i/o timeout"
                    .into(),
            ),
            ..Default::default()
        };
        assert_ne!(diagnose(&[a], &[]).cause, FailureCause::TimedOut);
    }

    /// The cause `diagnose` gives a step that exited 1 with `line` at the end of its log.
    fn cause_of_line(line: &str) -> FailureCause {
        diagnose(&[attempt(Some("app_error"), Some(1))], &log(&[line])).cause
    }

    #[test]
    fn a_damaged_or_foreign_input_is_bad_input() {
        for line in [
            "gzip: stdin: not in gzip format",
            "gzip: sample1_R1.fastq.gz: unexpected end of file",
            "[W::bam_hdr_read] EOF marker is absent. The input is probably truncated",
            "[E::bgzf_read] Read block operation failed with error 4 after 0 of 4 bytes",
            "pigz: skipping: sample1_R1.fastq.gz: corrupted -- crc32 mismatch",
            "gzip: sample1_R1.fastq.gz: invalid compressed data--crc error",
            "fastp: ERROR: sequence and quality have different length: invalid FASTQ record",
            "htsjdk.samtools.SAMFormatException: Malformed record: missing quality line",
            "Exception: Truncated gzip input stream",
            "gunzip: sample1_R1.fastq.gz: unexpected end of file",
            "zcat: stdin: unexpected end of file",
            "pigz: sample1_R1.fastq.gz: unexpected end of file",
            "[E::bgzf_uncompress] Inflate operation failed: invalid distance too far back",
            "htsjdk.samtools.SAMFormatException: Error parsing text SAM file. Not enough fields",
            "[E::vcf_parse] Malformed VCF line at chr1:100",
        ] {
            assert_eq!(cause_of_line(line), FailureCause::BadInput, "{line}");
        }
        let d = diagnose(
            &[attempt(Some("app_error"), Some(1))],
            &log(&["gzip: stdin: not in gzip format"]),
        );
        assert_eq!(
            d.message,
            "one of its input files looks damaged or is not in the format it expects"
        );
    }

    #[test]
    fn a_reference_that_does_not_match_is_reference_mismatch() {
        for line in [
            "[E::faidx_adjust_position] The sequence \"chr1\" was not found in reference",
            "A USER ERROR has occurred: Input files reads and reference have incompatible contigs: \
             No overlapping contigs found.",
            "java.lang.IllegalArgumentException: Sequence dictionaries are not the same size (25, 195)",
            "A USER ERROR has occurred: Contig 1 not present in the sequence dictionary [chr1, chr2]",
            "[mpileup] chromosome chr1 not in the reference index",
            "EXITING because of FATAL ERROR: Genome version: 2.7.4a is INCOMPATIBLE with running \
             STAR version: 2.7.10b",
            "ERROR: Sequence chrUn_KI270302v1 is not in the reference",
            "A USER ERROR has occurred: Input files reference and features have incompatible \
             contigs: The sequence dictionaries are not compatible",
            "[E::faidx_fetch] contig chrM not present in the reference",
        ] {
            assert_eq!(cause_of_line(line), FailureCause::ReferenceMismatch, "{line}");
        }
    }

    #[test]
    fn a_full_disk_is_no_space() {
        for line in [
            "samtools sort: failed to create temporary file \"/tmp/x.0001.bam\": No space left on \
             device",
            "cp: error writing 'out/aligned.bam': Disk quota exceeded",
        ] {
            assert_eq!(cause_of_line(line), FailureCause::NoSpace, "{line}");
        }
    }

    #[test]
    fn a_refused_read_or_write_is_access_denied() {
        for line in [
            "download failed: s3://lab-data/r1.fq.gz to ./r1.fq.gz An error occurred (AccessDenied) \
             when calling the GetObject operation: Access Denied",
            "fatal error: An error occurred (403) when calling the HeadObject operation: Forbidden",
            "curl: (22) The requested URL returned error: 403 Forbidden",
            "User: arn:aws:sts::1:assumed-role/run/x is not authorized to perform: s3:PutObject",
        ] {
            assert_eq!(cause_of_line(line), FailureCause::AccessDenied, "{line}");
        }
    }

    #[test]
    fn ordinary_lines_trip_none_of_the_new_log_causes() {
        for line in [
            "Output truncated to the first 1000 lines",
            "Verified crc32c checksum of s3://b/r1.fq.gz",
            "Genome version is compatible with current STAR",
            "Processing contig chr1 (248956422 bp)",
            "Filtered 403 reads with low quality",
            "Access to the cache granted",
            "Wrote 12 sequences to the reference index",
            "chromosome chr1: 1200 variants",
            "Free space on device: 120 GB",
            "failing on purpose",
            "/bin/bash: -c: line 1: syntax error: unexpected end of file",
            "ValueError: malformed node or string",
            "json.decoder.JSONDecodeError: malformed config at line 3",
            "Skipping chromosome chrM: not included in target regions",
            "12034 sites not in reference panel",
            "WARNING: 12 markers not found in the reference panel",
            "contig chrM: annotation not present, skipping",
        ] {
            assert_eq!(cause_of_line(line), FailureCause::StepError, "{line}");
        }
        // An engine's own config error stays an engine error.
        assert_eq!(
            diagnose_engine(&log(&[
                "WorkflowError: Invalid config: malformed YAML in config.yaml"
            ]))
            .cause,
            FailureCause::EngineError
        );
    }

    #[test]
    fn the_new_log_causes_rank_after_recorded_facts_and_missing_inputs() {
        // A recorded category outranks the log.
        let d =
            diagnose(&[attempt(Some("oom"), Some(1))], &log(&["gzip: stdin: not in gzip format"]));
        assert_eq!(d.cause, FailureCause::OutOfMemory);
        // A missing file is named before a damaged one.
        let d = diagnose(
            &[attempt(Some("app_error"), Some(1))],
            &log(&[
                "gzip: r2.fq.gz: No such file or directory",
                "gzip: stdin: unexpected end of file",
            ]),
        );
        assert_eq!(d.cause, FailureCause::MissingInput);
        // An explicit full disk outranks the truncated files it leaves behind, whichever line
        // comes first.
        for lines in [
            ["No space left on device", "gzip: stdin: unexpected end of file"],
            ["gzip: stdin: unexpected end of file", "No space left on device"],
        ] {
            let d = diagnose(&[attempt(Some("app_error"), Some(1))], &log(&lines));
            assert_eq!(d.cause, FailureCause::NoSpace, "{lines:?}");
        }
        // Then the table's order: a damaged input before a mismatched reference.
        let d = diagnose(
            &[attempt(Some("app_error"), Some(1))],
            &log(&[
                "ERROR: Sequence chr1 is not in the reference",
                "gzip: stdin: not in gzip format",
            ]),
        );
        assert_eq!(d.cause, FailureCause::BadInput);
    }

    #[test]
    fn an_engine_failure_with_no_log_is_unknown() {
        for tail in [log(&[]), log(&["", "  "])] {
            let d = diagnose_engine(&tail);
            assert_eq!(d.cause, FailureCause::Unknown, "{tail:?}");
            assert_eq!(d.cause.try_again(), TryAgain::Resume);
            assert!(d.advice.contains("coordinator"), "{}", d.advice);
        }
    }

    #[test]
    fn an_engine_failure_is_read_with_the_same_log_causes() {
        assert_eq!(
            diagnose_engine(&log(&[
                "ERROR ~ An error occurred (AccessDenied) when calling the GetObject operation"
            ]))
            .cause,
            FailureCause::AccessDenied
        );
    }

    #[test]
    fn an_engine_that_ran_out_of_memory_is_out_of_memory() {
        for line in [
            "Exception in thread \"main\" java.lang.OutOfMemoryError: Java heap space",
            "MemoryError",
        ] {
            let d = diagnose_engine(&log(&[line]));
            assert_eq!(d.cause, FailureCause::OutOfMemory, "{line}");
            assert!(d.message.contains("workflow engine"), "{}", d.message);
        }
        // A missing file named beside it still wins, as in `diagnose`.
        assert_eq!(
            diagnose_engine(&log(&[
                "java.lang.OutOfMemoryError: Java heap space",
                "ERROR ~ Path `s3://b/sheet.csv` does not exist",
            ]))
            .cause,
            FailureCause::MissingInput
        );
    }

    #[test]
    fn a_coordinator_s_recorded_stop_outranks_its_log() {
        let stop = |reason: &str, category: Option<&str>| FailedAttempt {
            exit_code: None,
            termination_category: category.map(str::to_string),
            status_reason: Some(reason.into()),
            spot_warned: false,
        };
        // Never started: no log at all, yet the reason says why.
        let d = diagnose_coordinator(
            &stop(
                "Task failed to start: CannotPullContainerError: pull image manifest has been \
                 retried 1 time(s): 123.dkr.ecr.us-east-1.amazonaws.com/biffix-runtime:sm-9 not \
                 found",
                None,
            ),
            &[],
        );
        assert_eq!(d.cause, FailureCause::CouldNotStart);
        assert!(d.message.contains("image"), "{}", d.message);
        let d = diagnose_coordinator(
            &stop("Task failed to start: CannotCreateContainerError: …", None),
            &[],
        );
        assert_eq!(d.cause, FailureCause::CouldNotStart);
        // A category recorded with the stop, or derived from its reason.
        let killed = stop("OutOfMemoryError: Container killed due to memory usage", None);
        assert_eq!(diagnose_coordinator(&killed, &[]).cause, FailureCause::OutOfMemory);
        let lost = stop("Host EC2 (instance i-1) terminated.", Some("host_terminated"));
        assert_eq!(diagnose_coordinator(&lost, &[]).cause, FailureCause::HostLost);
        // Nothing recorded: the engine's log decides, as for `diagnose_engine`.
        let plain = FailedAttempt { exit_code: Some(1), ..FailedAttempt::default() };
        assert_eq!(diagnose_coordinator(&plain, &[]).cause, FailureCause::Unknown);
        assert_eq!(
            diagnose_coordinator(&plain, &log(&["Error: bad parameter --foo"])).cause,
            FailureCause::EngineError
        );
    }

    #[test]
    fn each_cause_has_its_try_again_mode() {
        use FailureCause::*;
        for (cause, mode) in [
            (CouldNotStart, TryAgain::Resume),
            (OutOfMemory, TryAgain::Resume),
            (SpotReclaimed, TryAgain::Resume),
            (HostLost, TryAgain::Resume),
            (TimedOut, TryAgain::Resume),
            (MissingInput, TryAgain::FixSamples),
            (BadInput, TryAgain::FixSamples),
            (ReferenceMismatch, TryAgain::FixOptions),
            (NoSpace, TryAgain::Resume),
            (AccessDenied, TryAgain::FixSamples),
            (MissingOutput, TryAgain::Resume),
            (StepError, TryAgain::Resume),
            (EngineError, TryAgain::FixOptions),
            (LaunchFailed, TryAgain::Rerun),
            (Unknown, TryAgain::Resume),
        ] {
            assert_eq!(cause.try_again(), mode, "{cause:?}");
        }
        let names: Vec<&str> = TryAgain::ALL.into_iter().map(TryAgain::as_str).collect();
        assert_eq!(names, ["resume", "rerun", "fix_samples", "fix_options"]);
    }
}
