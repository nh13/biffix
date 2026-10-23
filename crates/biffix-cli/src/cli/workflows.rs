//! `biffix workflows …`: the workflow commands' flags, and their dispatch into
//! [`commands::workflows`].

use clap::Subcommand;

use crate::commands::{self, GlobalArgs, Session};
use crate::error::CliResult;
use crate::local::source::SourceMode;
use crate::render::OutputMode;

/// The longest `--wait-minutes` accepted: a week. A bound keeps the deadline arithmetic in range,
/// and makes a nonsensical value a usage error before anything is registered.
const MAX_WAIT_MINUTES: u64 = 7 * 24 * 60;

/// A `--wait-minutes` value as the wait it means.
fn wait_for(no_wait: bool, wait_minutes: u64) -> Option<std::time::Duration> {
    (!no_wait).then(|| std::time::Duration::from_secs(wait_minutes.saturating_mul(60)))
}

#[derive(Subcommand, Debug)]
pub(crate) enum WorkflowsCommand {
    /// Freeze an immutable workflow version from a local git checkout. The same source with the
    /// same inputs again is refused (exit 6) naming the version already registered; `reregister`
    /// reports that case as up to date instead.
    Register {
        /// Path to the workflow checkout (must be inside a git work tree; with `--source-mode git`,
        /// its top level, since runs clone the repository and run its root).
        path: String,
        /// Default worker image: an image reference (any registry; the server pins its digest), or
        /// a `sha256:…` digest of an image in the deploy's workflow repository. Optional — omit it
        /// when every rule/process declares its own container (Nextflow `container`, Snakemake's
        /// `aws_batch_container_image` resource). A Snakemake version without one runs its tasks
        /// on the coordinator's own image, which carries its pinned Snakemake.
        #[arg(long)]
        image: Option<String>,
        /// Workflow name (default: the directory name).
        #[arg(long)]
        name: Option<String>,
        /// The project to register in. Default: on a deploy of one project, that project; on a
        /// deploy of several, the one project the workflow's earlier versions are in, else
        /// --project is required. A deploy with no projects refuses the registration.
        #[arg(long)]
        project: Option<String>,
        /// Worker queue-class this version references (repeatable).
        #[arg(long = "queue-class")]
        queue_class: Vec<String>,
        /// Register even if the working tree is dirty (recorded in the output). Only with
        /// `--source-mode s3`, which stages the tree as it is; a git-mode run clones the commit.
        #[arg(long)]
        allow_dirty: bool,
        /// Engine version (default: biffix.yaml's engine_version).
        #[arg(long)]
        engine_version: Option<String>,
        /// Entry point, a git-tracked file relative to the root (a Nextflow one may be a
        /// directory). Default: biffix.yaml's entrypoint, else `main.nf`, or the first of
        /// `Snakefile`, `snakefile`, `workflow/Snakefile`, `workflow/snakefile` present, as
        /// Snakemake picks it.
        #[arg(long)]
        entrypoint: Option<String>,
        /// How the coordinator fetches the source at run time. `git` (default) records the origin
        /// for an anonymous clone — public repos only. `s3` stages an immutable source tarball to
        /// the project bucket and records that — REQUIRED for private repos, and better for
        /// reproducibility generally (no GitHub credential ever reaches the coordinator).
        #[arg(long, value_enum, default_value_t = SourceMode::Git)]
        source_mode: SourceMode,
        /// Return as soon as the version is registered, without waiting for its engine runtime and
        /// images to be built (by default the command waits until the version can run).
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for the version to be ready, in minutes (at most a week). A version
        /// still not ready then exits 7, as when the service is unavailable; it stays registered.
        #[arg(
            long,
            default_value_t = 30,
            conflicts_with = "no_wait",
            value_parser = clap::value_parser!(u64).range(1..=MAX_WAIT_MINUTES)
        )]
        wait_minutes: u64,
    },
    /// Preview the launch form a checkout would register with, without registering it. With
    /// `--output json` (or piped), the form as JSON — exactly what `register` sends.
    Form {
        /// Path to the workflow checkout.
        path: String,
        /// A manifest to use instead of the checkout's `biffix.yaml`.
        #[arg(long)]
        manifest: Option<String>,
        /// Open the form in the UI (the form travels in the URL; nothing is registered).
        #[arg(long)]
        open: bool,
    },
    /// Register workflow versions again from what they recorded, on this deployment's engine
    /// runtime, with their images pinned again (a tag that moved is pinned to where it is now).
    /// The form and About page stay as they were registered; to derive them again, register a
    /// checkout. A version with nothing new to register is reported as up to date.
    Reregister {
        /// A version id, or a workflow name (its newest version).
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        target: Option<String>,
        /// Every workflow's newest version (per project and name).
        #[arg(long)]
        all: bool,
        /// With --all, only workflows of this project. With a target, the project to register the
        /// new version in (default: the version's own): a workflow name is looked up there first,
        /// else in the one project that has it, and the new version is moved here (how a version
        /// labelled with a project this deploy does not configure is moved into one).
        #[arg(long)]
        project: Option<String>,
        /// An exact engine release to use instead of the recorded one (needed for a version that
        /// recorded only a major version, e.g. `9`).
        #[arg(long, conflicts_with = "all")]
        engine_version: Option<String>,
        /// Show what would be registered, without registering anything.
        #[arg(long)]
        dry_run: bool,
        /// Return as soon as each version is registered, without waiting until it can run.
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for each version to be ready, in minutes (at most a week). A version
        /// still not ready then stays registered: exit 7, as when the service is unavailable (with
        /// --all, it is reported as not ready, and the command exits 1).
        #[arg(
            long,
            default_value_t = 30,
            conflicts_with = "no_wait",
            value_parser = clap::value_parser!(u64).range(1..=MAX_WAIT_MINUTES)
        )]
        wait_minutes: u64,
    },
    /// List registered workflow versions, each marked released or developer.
    List {
        /// Only released versions.
        #[arg(long, conflicts_with = "developer")]
        released: bool,
        /// Only developer (not released) versions.
        #[arg(long)]
        developer: bool,
    },
    /// Release a workflow version: the Lab offers released versions by default, and launches a
    /// pipeline's newest released version. Needs the Developer or Admin role. Recorded with who
    /// and when; releasing a released version changes nothing. Version ids are in `biffix
    /// workflows list`.
    Release {
        /// Frozen version id.
        id: String,
    },
    /// Take a version's release back, so it is a developer version again. Needs the Developer or
    /// Admin role. Recorded with who and when; un-releasing a developer version changes nothing.
    Unrelease {
        /// Frozen version id.
        id: String,
    },
    /// Show one workflow version, including its parameter form.
    Show {
        /// Frozen version id (`{git_sha}:{content_hash}:{build}`, as `biffix workflows list` prints
        /// it).
        id: String,
    },
}

impl WorkflowsCommand {
    /// Run the parsed `workflows` command, resolving a session from `global` when it needs one.
    pub(super) async fn run(self, global: &GlobalArgs, output: OutputMode) -> CliResult<()> {
        let session = || Session::resolve(global);
        match self {
            // A form preview is local: it needs a deployment only to open the UI.
            WorkflowsCommand::Form { path, manifest, open } => {
                let session = open.then(session).transpose()?;
                let opts = commands::workflows::FormOpts { path, manifest, output };
                commands::workflows::form(session.as_ref(), opts)
            }
            WorkflowsCommand::Register {
                path,
                image,
                name,
                project,
                queue_class,
                allow_dirty,
                engine_version,
                entrypoint,
                source_mode,
                no_wait,
                wait_minutes,
            } => {
                commands::workflows::register(
                    &mut session()?,
                    commands::workflows::RegisterOpts {
                        path,
                        image,
                        name,
                        project,
                        queue_classes: queue_class,
                        allow_dirty,
                        engine_version,
                        entrypoint,
                        source_mode,
                        wait: wait_for(no_wait, wait_minutes),
                    },
                )
                .await
            }
            WorkflowsCommand::Reregister {
                target,
                all,
                project,
                engine_version,
                dry_run,
                no_wait,
                wait_minutes,
            } => {
                commands::workflows::reregister(
                    &mut session()?,
                    commands::workflows::ReregisterOpts {
                        target,
                        all,
                        project,
                        engine_version,
                        dry_run,
                        wait: wait_for(no_wait, wait_minutes),
                    },
                )
                .await
            }
            WorkflowsCommand::List { released, developer } => {
                // `--released` / `--developer` (mutually exclusive); neither lists every one.
                commands::workflows::list(&mut session()?, super::flag_pair(released, developer))
                    .await
            }
            WorkflowsCommand::Release { id } => {
                commands::workflows::release(&mut session()?, &id, true).await
            }
            WorkflowsCommand::Unrelease { id } => {
                commands::workflows::release(&mut session()?, &id, false).await
            }
            WorkflowsCommand::Show { id } => commands::workflows::show(&mut session()?, &id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn flag_rules_are_enforced_by_clap() {
        use clap::error::ErrorKind;
        let kind = |argv: &[&str]| {
            let mut full = vec!["biffix"];
            full.extend_from_slice(argv);
            Cli::try_parse_from(full).map(|_| ()).unwrap_err().kind()
        };
        assert_eq!(kind(&["workflows", "reregister"]), ErrorKind::MissingRequiredArgument);
        assert_eq!(kind(&["workflows", "reregister", "wf", "--all"]), ErrorKind::ArgumentConflict);
        assert_eq!(
            kind(&["workflows", "register", ".", "--no-wait", "--wait-minutes", "5"]),
            ErrorKind::ArgumentConflict
        );
        // A wait long enough to overflow the deadline is a usage error, not a panic after the
        // version is registered.
        for huge in ["0", "10081", "18446744073709551615"] {
            assert_eq!(
                kind(&["workflows", "register", ".", "--wait-minutes", huge]),
                ErrorKind::ValueValidation,
                "{huge}"
            );
        }
        assert_eq!(
            wait_for(false, MAX_WAIT_MINUTES),
            Some(std::time::Duration::from_secs(MAX_WAIT_MINUTES * 60))
        );
        assert_eq!(wait_for(true, 30), None);
    }

    #[test]
    fn release_and_unrelease_take_a_version_id() {
        use clap::error::ErrorKind;
        let parse = |argv: &[&str]| {
            let mut full = vec!["biffix", "workflows"];
            full.extend_from_slice(argv);
            Cli::try_parse_from(full).map(|cli| match cli.command {
                Command::Workflows { command: WorkflowsCommand::Release { id } } => ("release", id),
                Command::Workflows { command: WorkflowsCommand::Unrelease { id } } => {
                    ("unrelease", id)
                }
                _ => unreachable!(),
            })
        };
        assert_eq!(parse(&["release", "a:h:0"]).unwrap(), ("release", "a:h:0".to_string()));
        assert_eq!(parse(&["unrelease", "a:h:0"]).unwrap(), ("unrelease", "a:h:0".to_string()));
        assert_eq!(parse(&["release"]).unwrap_err().kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn workflows_list_filters_on_released_or_developer_but_not_both() {
        use clap::error::ErrorKind;
        let parse = |argv: &[&str]| {
            let mut full = vec!["biffix", "workflows", "list"];
            full.extend_from_slice(argv);
            Cli::try_parse_from(full).map(|cli| match cli.command {
                Command::Workflows { command: WorkflowsCommand::List { released, developer } } => {
                    (released, developer)
                }
                _ => unreachable!(),
            })
        };
        assert_eq!(parse(&[]).unwrap(), (false, false));
        assert_eq!(parse(&["--released"]).unwrap(), (true, false));
        assert_eq!(parse(&["--developer"]).unwrap(), (false, true));
        assert_eq!(
            parse(&["--released", "--developer"]).unwrap_err().kind(),
            ErrorKind::ArgumentConflict
        );
    }

    /// The `register --project` help says what `choose_project` does: no fallback to the workflow
    /// name, which no deploy has.
    #[test]
    fn register_project_help_describes_the_default_project() {
        use clap::CommandFactory;
        let mut cli = Cli::command();
        let register = cli
            .find_subcommand_mut("workflows")
            .and_then(|w| w.find_subcommand_mut("register"))
            .unwrap();
        let help = register
            .get_arguments()
            .find(|a| a.get_id() == "project")
            .and_then(|a| a.get_help())
            .unwrap()
            .to_string();
        assert!(!help.contains("workflow name"), "{help}");
        for said in [
            "one project, that project",
            "earlier versions",
            "--project is required",
            "no projects",
        ] {
            assert!(help.contains(said), "{said}: {help}");
        }
    }

    #[test]
    fn source_modes_have_their_wire_names() {
        assert_eq!(SourceMode::Git.as_str(), "git");
        assert_eq!(SourceMode::S3.as_str(), "s3");
    }
}
