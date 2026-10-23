//! `biffix` — the CLI entry point. Parses args, dispatches, and maps the classified error to the
//! documented exit code. clap usage errors exit 2 on their own.

use clap::Parser;

use biffix_cli::cli::Cli;
use biffix_cli::render::print_error;

#[tokio::main]
async fn main() {
    restore_default_sigpipe();
    let cli = Cli::parse();
    let output = cli.output_mode();
    match cli.run().await {
        Ok(()) => std::process::exit(biffix_cli::error::exit::SUCCESS),
        Err(err) => {
            print_error(&err, output);
            std::process::exit(err.exit_code());
        }
    }
}

/// Rust ignores SIGPIPE, so every `println!` into a closed pipe (`biffix runs list | head -1`)
/// panics. Restoring the default disposition ends the process quietly the moment its reader goes
/// away, as any Unix filter does — for every output site at once.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // SAFETY: `signal` with `SIG_DFL` only changes this process's disposition for SIGPIPE; it is
    // process-wide and takes no pointers, and it runs first thing in `main`, before any output.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}
