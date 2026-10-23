//! The clap command/flag surface, and dispatch into the command implementations: the top-level
//! commands here, `biffix workflows …` in `workflows.rs` and `biffix runs …` in `runs.rs`.

mod runs;
mod workflows;

use clap::{Parser, Subcommand};

use crate::commands::{self, GlobalArgs, Session};
use crate::error::CliResult;

pub use crate::local::source::SourceMode;
use runs::RunsCommand;
use workflows::WorkflowsCommand;

#[derive(Parser, Debug)]
#[command(
    name = "biffix",
    version,
    about = "biffix (say \"BFX\", short for bioinformatics): register and run workflows from the \
             command line."
)]
pub struct Cli {
    #[command(flatten)]
    pub(crate) global: GlobalArgs,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Sign in to a deployment and cache credentials for the profile (in ~/.config/biffix, or
    /// $XDG_CONFIG_HOME/biffix; $BIFFIX_CONFIG_DIR overrides both).
    Login {
        /// Do not open a browser; print the URL and accept a pasted redirect URL/code.
        #[arg(long)]
        no_browser: bool,
    },
    /// Clear cached credentials for the active profile.
    Logout,
    /// Show the signed-in identity and confirm the token is accepted.
    Whoami,
    /// Probe the deployment's liveness/readiness (and component status when signed in).
    Health,
    /// List the engines the deployment runs and the versions a workflow may pin.
    Engines,
    /// Find the container images a workflow's tasks name (and, for Nextflow, the plugins it
    /// declares); `--write` adds the images `biffix.yaml` does not declare yet to its `images:`
    /// block (all `workflows register` imports) and sets its `plugins:`.
    ///
    /// Nextflow runs `nextflow inspect` (the `nextflow` on your PATH; arguments after `--`, e.g.
    /// `-profile docker`, go to it; run once per profile or params that change which processes run)
    /// and `nextflow config` (given only the `-profile`) for the pipeline's plugins; if that fails,
    /// it warns and leaves `plugins:` alone. Snakemake reads the workflow's files for
    /// `aws_batch_container_image`; values that are not plain strings are listed for you to declare
    /// by hand.
    Inspect {
        /// Path to the workflow checkout.
        #[arg(default_value = ".")]
        path: String,
        /// Add the images found to `images:` in biffix.yaml and, for Nextflow, set `plugins:` to
        /// the plugins the pipeline declares (replacing the list; only those two blocks are
        /// edited).
        #[arg(long)]
        write: bool,
        /// Arguments for `nextflow inspect` (after `--`; only `-profile` also reaches
        /// `nextflow config`).
        #[arg(last = true)]
        nextflow_args: Vec<String>,
    },
    /// Register, re-register, release, list or show workflow versions, or preview a launch form.
    Workflows {
        #[command(subcommand)]
        command: WorkflowsCommand,
    },
    /// Submit, list, inspect, or follow runs.
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
}

impl Cli {
    /// Build a session and dispatch the parsed command.
    pub async fn run(self) -> CliResult<()> {
        let output = self.output_mode();
        let Cli { global, command } = self;
        match command {
            // Inspecting is local: it reads the checkout (and runs `nextflow inspect`), nothing
            // else.
            Command::Inspect { path, write, nextflow_args } => {
                commands::inspect::inspect(commands::inspect::InspectOpts {
                    path,
                    write,
                    nextflow_args,
                    output,
                })
            }
            // Logging out is local too: it needs only the profile name, never an API URL.
            Command::Logout => commands::login::logout(&global),
            // A form preview needs no session either; the workflows dispatch resolves its own.
            Command::Workflows { command } => command.run(&global, output).await,
            // Every other command resolves a session first.
            Command::Login { no_browser } => {
                commands::login::login(&mut Session::resolve(&global)?, no_browser).await
            }
            Command::Whoami => commands::login::whoami(&mut Session::resolve(&global)?).await,
            Command::Health => commands::health::health(&mut Session::resolve(&global)?).await,
            Command::Engines => commands::workflows::engines(&mut Session::resolve(&global)?).await,
            Command::Runs { command } => command.run(&mut Session::resolve(&global)?).await,
        }
    }

    /// The resolved output mode (for error rendering in `main`).
    pub fn output_mode(&self) -> crate::render::OutputMode {
        crate::render::OutputMode::resolve(self.global.output)
    }
}

/// The choice an on/off flag pair makes: `Some(true)` for the on flag, `Some(false)` for the off
/// flag, `None` (the default) for neither. Where both may be given, the last wins through clap's
/// `overrides_with`, so at most one is set here.
fn flag_pair(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
