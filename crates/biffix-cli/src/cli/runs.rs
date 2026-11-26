//! `biffix runs …`: the run commands' flags, and their dispatch into [`commands::runs`].

use biffix_api::RunFilter;
use clap::{Subcommand, ValueEnum};

use crate::commands::{self, Session};
use crate::error::CliResult;

/// What happens to a run's intermediate files (a Nextflow run's `work/`) once it ends.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum RetentionArg {
    /// Deleted the deploy's retention period after the run ends.
    Default,
    /// Deleted as soon as the run succeeds (it can then no longer be resumed).
    DeleteOnSuccess,
    /// Never deleted.
    Keep,
}

impl From<RetentionArg> for biffix_core::event::Retention {
    fn from(r: RetentionArg) -> Self {
        match r {
            RetentionArg::Default => Self::Default,
            RetentionArg::DeleteOnSuccess => Self::DeleteOnSuccess,
            RetentionArg::Keep => Self::Keep,
        }
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "parsed once per invocation, so the size gap between `Submit` (many flags) and the \
              rest costs nothing; boxing a clap subcommand's fields would only obscure it"
)]
#[derive(Subcommand, Debug)]
pub(crate) enum RunsCommand {
    /// Submit a run of a registered version.
    Submit {
        /// Frozen workflow version id.
        version: String,
        /// Params file (YAML or JSON).
        #[arg(long = "params")]
        params_file: Option<String>,
        /// Inline `key=value` param override (repeatable; overrides the file).
        #[arg(long = "param")]
        param: Vec<String>,
        /// Target to build (engine-agnostic): a Snakemake target rule/output, or a Nextflow
        /// `-entry` workflow. Overrides the version's registered target for this run, so one frozen
        /// version can drive different targets (e.g. a quick `smoke` vs the full `all`).
        #[arg(long)]
        target: Option<String>,
        /// Output prefix: the workflow's default storage location (must be within the project
        /// bucket). Overrides the isolated per-run default so outputs land at a caller-chosen stable
        /// prefix — enabling incremental re-runs.
        #[arg(long)]
        output_prefix: Option<String>,
        /// Default number of times to retry a failed task (e.g. after a spot reclaim), at most 10.
        /// A default only: a Snakemake rule's own `retries:` and a Nextflow pipeline's own
        /// `errorStrategy` still win. Omit for the deploy's default (3).
        #[arg(long)]
        retries: Option<u32>,
        /// Keep running the rest of the workflow after a task fails (Snakemake `--keep-going`,
        /// the default). Nextflow has no equivalent and ignores it.
        #[arg(long, overrides_with = "no_keep_going")]
        keep_going: bool,
        /// Stop the workflow at the first failed task.
        #[arg(long, overrides_with = "keep_going")]
        no_keep_going: bool,
        /// Resume a finished run of the same workflow: re-use its output prefix (and, for
        /// Nextflow, its work dir and cache; for Snakemake, its outputs and run metadata) so
        /// finished, unchanged steps are skipped. Its params are inherited (except secrets,
        /// which must be passed again); --params and --param override them.
        #[arg(long, value_name = "RUN_ID")]
        resume_from: Option<String>,
        /// A human name for the run, shown in run lists and on the run page.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// A reference bundle id (see Settings → References); fills the form's reference params.
        #[arg(long, value_name = "BUNDLE")]
        reference: Option<String>,
        /// What happens to the run's intermediate files once it ends (Nextflow only). Results are
        /// never deleted; a run whose intermediate files are gone can no longer be resumed.
        #[arg(long, value_enum)]
        retention: Option<RetentionArg>,
        /// Container image a task may name, pinned now and imported into the tenant (repeatable).
        /// An image not yet imported into the deploy needs the Developer role; one already
        /// imported (such as a prior run's) needs only the Operator role. The run waits in
        /// `preparing_images` until every one is runnable. Wins over a version image spelled the
        /// same (`REF@sha256:…` also wins `REF`). A resumed run inherits the prior run's images,
        /// each as `<spelling>@<digest it ran>`, unless this is given.
        #[arg(long = "image", value_name = "REF")]
        image: Vec<String>,
        /// Launch the run into this experiment (an experiment id of the version's project that is
        /// not archived). A resumed run is launched into the prior run's experiment unless this
        /// or --no-experiment is given (and that experiment is still open).
        #[arg(long, value_name = "EXPERIMENT_ID", conflicts_with = "no_experiment")]
        experiment: Option<String>,
        /// Launch the run outside any experiment, even when resuming a run that is in one.
        #[arg(long)]
        no_experiment: bool,
        /// Run the whole workflow as one AWS Batch job on one host (single-job mode): every task
        /// runs on that host in the image its Batch job would run, its working files on the host's
        /// local disk (a Snakemake run still publishes every output to its output prefix). The
        /// version must allow it (`single_job:` in its biffix.yaml), which also sets the default
        /// size; --instance or --cpus/--memory-gib override it. Cannot be combined with
        /// --resume-from: a single-job run cannot be a resume, its work directory being on one
        /// host's local disk.
        #[arg(long, conflicts_with = "resume_from")]
        single_job: bool,
        /// Single-job: the instance type used to size the whole-host resource request (one of the
        /// queue-class's), e.g. `c6id.8xlarge`. Batch may place the run on another same-size type
        /// in that queue-class.
        #[arg(
            long,
            value_name = "TYPE",
            requires = "single_job",
            conflicts_with_all = ["cpus", "memory_gib"]
        )]
        instance: Option<String>,
        /// Single-job: at least this many vCPUs (with --memory-gib); the smallest instance type of
        /// the queue-class with both is used.
        #[arg(long, value_name = "N", requires_all = ["single_job", "memory_gib"])]
        cpus: Option<u32>,
        /// Single-job: at least this much memory for the run's tasks, in GiB (with --cpus). It is
        /// compared with what the tasks get, not the instance type's nominal memory: the host's
        /// memory less a reserve for the operating system and ECS (about 6%), less what the
        /// engine keeps for itself (Snakemake 1/16 of the rest, 2 to 8 GiB, plus 160 MiB a vCPU
        /// for each job's own Snakemake; Nextflow its heap and JVM slack, about 3/32, at least 2
        /// GiB). A c6id.8xlarge (64 GiB, 32 vCPUs) leaves its tasks 51 GiB under Snakemake, 54 GiB
        /// under Nextflow.
        #[arg(long, value_name = "GIB", requires_all = ["single_job", "cpus"])]
        memory_gib: Option<u32>,
        /// Single-job: the deploy's single-job queue-class to run on (default: the version's, else
        /// the deploy's first).
        #[arg(long, value_name = "NAME", requires = "single_job")]
        single_job_queue_class: Option<String>,
        /// Follow the run to a terminal state (exit 0 on success, 8 failed, 9 cancelled, 1 a
        /// terminal state this biffix does not know); a failed run's reason is shown.
        #[arg(long)]
        follow: bool,
    },
    /// List runs.
    List {
        /// Filter by run state: the server's own filter set (biffix-api's `RunFilter`), so clap
        /// rejects any value the server would 400 on.
        #[arg(long, value_enum)]
        state: Option<RunFilter>,
    },
    /// Show one run with per-task state, and why it failed when it did.
    Show {
        /// Run id.
        run: String,
    },
    /// Read the coordinator's log, or a task's display-attempt log, through biffix authentication.
    Logs {
        /// Run id.
        run: String,
        /// Task key (omit to read the coordinator).
        #[arg(long)]
        task: Option<String>,
        /// Poll until this log's source ends, then exit 0 regardless of the run outcome.
        /// An older backend cannot report the ending: interrupt with Ctrl-C to stop.
        #[arg(long)]
        follow: bool,
    },
    /// Follow a run to a terminal state (exit 0 on success, 8 failed, 9 cancelled, 1 a terminal
    /// state this biffix does not know); a failed run's reason is shown.
    Follow {
        /// Run id.
        run: String,
    },
    /// Cancel a run: stop its coordinator (its remaining jobs are then terminated) and mark it
    /// cancelled. A no-op for a finished run.
    Cancel {
        /// Run id.
        run: String,
    },
    /// Put a run in an experiment of its project, move it to another, or take it out (--none).
    Experiment {
        /// Run id.
        run: String,
        /// The experiment to put it in (an experiment id of the run's project that is not
        /// archived).
        #[arg(
            value_name = "EXPERIMENT_ID",
            required_unless_present = "none",
            conflicts_with = "none"
        )]
        experiment: Option<String>,
        /// Take the run out of its experiment.
        #[arg(long)]
        none: bool,
    },
    /// Rename a run, or clear its name (--clear) so the UI shows a name derived from its pipeline,
    /// date and samples.
    Rename {
        /// Run id.
        run: String,
        /// The new name (at most 200 characters). A blank name is refused: use --clear to clear
        /// the name.
        #[arg(value_name = "NAME", required_unless_present = "clear", conflicts_with = "clear")]
        name: Option<String>,
        /// Clear the run's name.
        #[arg(long)]
        clear: bool,
    },
}

impl RunsCommand {
    /// Run the parsed `runs` command in `session`.
    pub(super) async fn run(self, session: &mut Session) -> CliResult<()> {
        match self {
            RunsCommand::Submit {
                version,
                params_file,
                param,
                target,
                output_prefix,
                retries,
                keep_going,
                no_keep_going,
                resume_from,
                name,
                reference,
                retention,
                image,
                experiment,
                no_experiment,
                single_job,
                instance,
                cpus,
                memory_gib,
                single_job_queue_class,
                follow,
            } => {
                commands::runs::submit(
                    session,
                    commands::runs::SubmitOpts {
                        version_id: version,
                        params_file,
                        inline: param,
                        target,
                        output_prefix,
                        retries,
                        keep_going: super::flag_pair(keep_going, no_keep_going),
                        resume_from,
                        run_name: name,
                        reference,
                        retention: retention.map(Into::into),
                        images: image,
                        experiment: commands::runs::ExperimentChoice::of(experiment, no_experiment),
                        single_job: single_job.then_some(biffix_api::SingleJobRequest {
                            queue_class: single_job_queue_class,
                            instance,
                            cpus,
                            memory_gib,
                        }),
                        follow,
                    },
                )
                .await
            }
            RunsCommand::List { state } => commands::runs::list(session, state).await,
            RunsCommand::Show { run } => commands::runs::show(session, &run).await,
            RunsCommand::Logs { run, task, follow } => {
                commands::runs_logs::logs(session, &run, task.as_deref(), follow).await
            }
            RunsCommand::Follow { run } => commands::runs::follow(session, &run).await,
            RunsCommand::Cancel { run } => commands::runs::cancel(session, &run).await,
            RunsCommand::Experiment { run, experiment, none: _ } => {
                commands::runs::set_experiment(session, &run, experiment.as_deref()).await
            }
            RunsCommand::Rename { run, name, clear: _ } => {
                commands::runs::rename(session, &run, name.as_deref()).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn keep_going_of(args: &[&str]) -> Option<bool> {
        let mut argv = vec!["biffix", "runs", "submit", "v1"];
        argv.extend_from_slice(args);
        match Cli::parse_from(argv).command {
            Command::Runs { command: RunsCommand::Submit { keep_going, no_keep_going, .. } } => {
                crate::cli::flag_pair(keep_going, no_keep_going)
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn keep_going_flags_resolve_to_the_last_one_given() {
        assert_eq!(keep_going_of(&[]), None);
        assert_eq!(keep_going_of(&["--keep-going"]), Some(true));
        assert_eq!(keep_going_of(&["--no-keep-going"]), Some(false));
        assert_eq!(keep_going_of(&["--keep-going", "--no-keep-going"]), Some(false));
        assert_eq!(keep_going_of(&["--no-keep-going", "--keep-going"]), Some(true));
    }

    fn single_job_of(args: &[&str]) -> Result<Option<biffix_api::SingleJobRequest>, clap::Error> {
        let mut argv = vec!["biffix", "runs", "submit", "v1"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv)?.command {
            Command::Runs {
                command:
                    RunsCommand::Submit {
                        single_job,
                        instance,
                        cpus,
                        memory_gib,
                        single_job_queue_class,
                        ..
                    },
            } => Ok(single_job.then_some(biffix_api::SingleJobRequest {
                queue_class: single_job_queue_class,
                instance,
                cpus,
                memory_gib,
            })),
            _ => unreachable!(),
        }
    }

    #[test]
    fn single_job_takes_an_instance_or_cpus_with_memory() {
        assert_eq!(single_job_of(&[]).unwrap(), None);
        assert_eq!(single_job_of(&["--single-job"]).unwrap(), Some(Default::default()));
        let by_type = single_job_of(&["--single-job", "--instance", "c6id.8xlarge"]).unwrap();
        assert_eq!(by_type.unwrap().instance.as_deref(), Some("c6id.8xlarge"));
        let sized = single_job_of(&["--single-job", "--cpus", "32", "--memory-gib", "64"])
            .unwrap()
            .unwrap();
        assert_eq!((sized.cpus, sized.memory_gib), (Some(32), Some(64)));
        let named = single_job_of(&["--single-job", "--single-job-queue-class", "big"]).unwrap();
        assert_eq!(named.unwrap().queue_class.as_deref(), Some("big"));
        for bad in [
            &["--instance", "c6id.8xlarge"][..],
            &["--single-job", "--cpus", "32"],
            &["--single-job", "--memory-gib", "64"],
            &["--single-job", "--instance", "c6id.8xlarge", "--cpus", "32", "--memory-gib", "64"],
            &["--single-job", "--resume-from", "run-1"],
            &["--single-job-queue-class", "big"],
        ] {
            assert!(single_job_of(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_unknown_run_state_is_a_usage_error() {
        let err = Cli::try_parse_from(["biffix", "runs", "list", "--state", "nope"])
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn resume_from_parses_a_run_id() {
        let resume_of = |args: &[&str]| {
            let mut argv = vec!["biffix", "runs", "submit", "v1"];
            argv.extend_from_slice(args);
            match Cli::parse_from(argv).command {
                Command::Runs { command: RunsCommand::Submit { resume_from, .. } } => resume_from,
                _ => unreachable!(),
            }
        };
        assert_eq!(resume_of(&[]), None);
        assert_eq!(resume_of(&["--resume-from", "run-abc"]).as_deref(), Some("run-abc"));
    }

    #[test]
    fn experiment_flags_parse_to_a_choice() {
        use commands::runs::ExperimentChoice;
        let choice_of = |args: &[&str]| {
            let mut argv = vec!["biffix", "runs", "submit", "v1"];
            argv.extend_from_slice(args);
            match Cli::parse_from(argv).command {
                Command::Runs {
                    command: RunsCommand::Submit { experiment, no_experiment, .. },
                } => ExperimentChoice::of(experiment, no_experiment),
                _ => unreachable!(),
            }
        };
        assert_eq!(choice_of(&[]), ExperimentChoice::Unset);
        assert_eq!(choice_of(&["--experiment", "exp-1"]), ExperimentChoice::Into("exp-1".into()));
        assert_eq!(choice_of(&["--no-experiment"]), ExperimentChoice::Outside);
        let err = Cli::try_parse_from([
            "biffix",
            "runs",
            "submit",
            "v1",
            "--experiment",
            "e",
            "--no-experiment",
        ])
        .map(|_| ())
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn runs_experiment_takes_an_id_or_none() {
        use clap::error::ErrorKind;
        let parse = |args: &[&str]| {
            let mut argv = vec!["biffix", "runs", "experiment"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.command {
                Command::Runs { command: RunsCommand::Experiment { run, experiment, .. } } => {
                    (run, experiment)
                }
                _ => unreachable!(),
            })
        };
        assert_eq!(parse(&["r1", "exp-1"]).unwrap(), ("r1".to_string(), Some("exp-1".to_string())));
        assert_eq!(parse(&["r1", "--none"]).unwrap(), ("r1".to_string(), None));
        assert_eq!(parse(&["r1"]).unwrap_err().kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(
            parse(&["r1", "exp-1", "--none"]).unwrap_err().kind(),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn runs_rename_takes_a_name_or_clear() {
        use clap::error::ErrorKind;
        let parse = |args: &[&str]| {
            let mut argv = vec!["biffix", "runs", "rename"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.command {
                Command::Runs { command: RunsCommand::Rename { run, name, .. } } => (run, name),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            parse(&["r1", "Liver batch 2"]).unwrap(),
            ("r1".to_string(), Some("Liver batch 2".to_string()))
        );
        assert_eq!(parse(&["r1", "--clear"]).unwrap(), ("r1".to_string(), None));
        assert_eq!(parse(&["r1"]).unwrap_err().kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(parse(&["r1", "x", "--clear"]).unwrap_err().kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn image_is_repeatable() {
        let images_of = |args: &[&str]| {
            let mut argv = vec!["biffix", "runs", "submit", "v1"];
            argv.extend_from_slice(args);
            match Cli::parse_from(argv).command {
                Command::Runs { command: RunsCommand::Submit { image, .. } } => image,
                _ => unreachable!(),
            }
        };
        assert!(images_of(&[]).is_empty());
        assert_eq!(
            images_of(&["--image", "quay.io/b/x:1", "--image", "sha256:abc"]),
            ["quay.io/b/x:1", "sha256:abc"]
        );
    }
}
