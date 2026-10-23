//! Presentation: human-readable when stdout is a TTY, structured JSON when piped or `--output
//! json`. Diagnostics go to stderr so JSON on stdout stays machine-parseable.

use std::io::IsTerminal;

use clap::ValueEnum;
use serde::Serialize;

use crate::error::CliError;

/// The `--output` flag's three choices. `Auto` consults the terminal; `Human`/`Json` are explicit
/// and force their mode regardless of whether stdout is a TTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human when stdout is a TTY, JSON when piped. The default (matches clap's `default_value`).
    #[default]
    Auto,
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

impl OutputMode {
    /// Resolve the effective mode from the chosen [`OutputFormat`]. `Json`/`Human` force their mode
    /// unconditionally; only `Auto` falls back to TTY detection (JSON when stdout is piped). The
    /// bug this shape prevents: collapsing the three-way choice to a "json?" bool made `--output
    /// human` indistinguishable from `Auto`, so a piped `--output human` silently emitted JSON.
    pub(crate) fn resolve(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Json => OutputMode::Json,
            OutputFormat::Human => OutputMode::Human,
            OutputFormat::Auto => {
                if std::io::stdout().is_terminal() {
                    OutputMode::Human
                } else {
                    OutputMode::Json
                }
            }
        }
    }
}

/// Print a value as pretty JSON to stdout.
pub(crate) fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("[biffix] could not serialize output: {e}"),
    }
}

/// Emit a classified error: JSON `{ "error": kind, "message": … }` to stderr in JSON mode, or a
/// prefixed line in human mode. Never prints a bare stack trace as the primary output.
pub fn print_error(err: &CliError, mode: OutputMode) {
    match mode {
        OutputMode::Json => {
            let body = serde_json::json!({ "error": err.kind(), "message": err.to_string() });
            eprintln!("{}", serde_json::to_string(&body).unwrap_or_else(|_| err.to_string()));
        }
        OutputMode::Human => eprintln!("error: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_format_forces_its_mode() {
        // The regression the `resolve` doc names: an explicit `--output human` must stay human even
        // when stdout is piped (as it is under the test harness), and `json` stays JSON. `Auto`
        // depends on the terminal, so it is not pinned here.
        assert_eq!(OutputMode::resolve(OutputFormat::Human), OutputMode::Human);
        assert_eq!(OutputMode::resolve(OutputFormat::Json), OutputMode::Json);
    }
}
