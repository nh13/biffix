//! Presentation: human-readable when stdout is a TTY, structured JSON when piped or `--output
//! json`. Diagnostics go to stderr so JSON on stdout stays machine-parseable.
//!
//! Everything the CLI prints goes through `tprint!`, `tprintln!`, `teprint!` and
//! `teprintln!`: `print!` and its siblings with control characters removed (`terminal_text`),
//! as most lines carry text from the server or a workflow. The std macros are disallowed in this
//! crate (`clippy.toml`).

use std::io::{IsTerminal, Write};

/// `print!` to stdout, terminal-safe ([`terminal_text`]).
macro_rules! tprint {
    ($($arg:tt)*) => {
        { $crate::render::write_terminal(::std::io::stdout(), format_args!($($arg)*), false); }
    };
}

/// `println!` to stdout, terminal-safe ([`terminal_text`]).
macro_rules! tprintln {
    ($($arg:tt)*) => {
        { $crate::render::write_terminal(::std::io::stdout(), format_args!($($arg)*), true); }
    };
}

/// `eprint!` to stderr, terminal-safe ([`terminal_text`]).
macro_rules! teprint {
    ($($arg:tt)*) => {
        { $crate::render::write_terminal(::std::io::stderr(), format_args!($($arg)*), false); }
    };
}

/// `eprintln!` to stderr, terminal-safe ([`terminal_text`]).
macro_rules! teprintln {
    ($($arg:tt)*) => {
        { $crate::render::write_terminal(::std::io::stderr(), format_args!($($arg)*), true); }
    };
}

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

/// Text from a server or a workflow as it may be printed to a terminal: control characters removed
/// (an escape sequence in it must not drive the user's terminal: clear it, move the cursor, set
/// its title), and tab too unless `keep_tab` (a log line's own columns).
pub(crate) fn terminal_safe(text: &str, keep_tab: bool) -> String {
    text.chars().filter(|c| (keep_tab && *c == '\t') || !c.is_control()).collect()
}

/// Text the CLI writes to a terminal: as [`terminal_safe`], but line breaks and tabs are kept (the
/// CLI's own multi-line and columned output). A carriage return is removed: it could overwrite the
/// line it ends.
pub(crate) fn terminal_text(text: &str) -> String {
    text.chars().filter(|c| matches!(c, '\n' | '\t') || !c.is_control()).collect()
}

/// Writes `args` to `out` as [`terminal_text`], then a line break if `newline`: what the
/// [`tprint!`] family expands to. Panics if the write fails, as `print!` does.
pub(crate) fn write_terminal<W: Write>(mut out: W, args: std::fmt::Arguments<'_>, newline: bool) {
    let mut text = terminal_text(&args.to_string());
    if newline {
        text.push('\n');
    }
    if let Err(e) = out.write_all(text.as_bytes()) {
        panic!("failed printing to the terminal: {e}");
    }
}

/// Escape literal controls left by JSON serialization (DEL/C1) without changing decoded keys or
/// values. C0 controls are already escaped by serde_json; structural newlines remain readable.
fn terminal_json(serialized: &str) -> String {
    use std::fmt::Write as _;

    let mut escaped = String::with_capacity(serialized.len());
    for c in serialized.chars() {
        if c.is_control() && !matches!(c, '\n' | '\t' | '\r') {
            write!(&mut escaped, "\\u{:04x}", c as u32).expect("writing to a String cannot fail");
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// Print a value as pretty JSON to stdout, escaping terminal controls without changing its data.
pub(crate) fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => tprintln!("{}", terminal_json(&s)),
        Err(e) => teprintln!("[biffix] could not serialize output: {e}"),
    }
}

/// Emit a classified error: JSON `{ "error": kind, "message": … }` to stderr in JSON mode, or a
/// prefixed line in human mode. Never prints a bare stack trace as the primary output.
pub fn print_error(err: &CliError, mode: OutputMode) {
    match mode {
        OutputMode::Json => {
            let body = serde_json::json!({ "error": err.kind(), "message": err.to_string() });
            teprintln!(
                "{}",
                terminal_json(
                    &serde_json::to_string(&body).expect("a JSON value always serializes")
                )
            );
        }
        OutputMode::Human => teprintln!("error: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_safe_text_drops_control_characters() {
        assert_eq!(terminal_safe("\u{1b}[31mred\u{1b}[0m\tdone\r\u{7}", true), "[31mred[0m\tdone");
        assert_eq!(terminal_safe("\u{1b}[31mred\u{1b}[0m\tdone\r\u{7}", false), "[31mred[0mdone");
        assert_eq!(terminal_safe("échantillon → ok", true), "échantillon → ok");
    }

    #[test]
    fn terminal_text_keeps_line_breaks_and_tabs_only() {
        let text = "\u{1b}]0;title\u{7}run\tfailed\r\n\u{9b}2Jnext\u{8}\u{85}line\n";
        assert_eq!(terminal_text(text), "]0;titlerun\tfailed\n2Jnextline\n");
        assert_eq!(terminal_text("échantillon → ok"), "échantillon → ok");
    }

    #[test]
    fn the_terminal_writer_filters_text_and_preserves_newline_choice() {
        let mut out = Vec::new();
        write_terminal(&mut out, format_args!("{} → {}", "run-\u{1b}[2J1", "failed"), true);
        write_terminal(&mut out, format_args!("a\tb\u{7}"), false);
        assert_eq!(String::from_utf8(out).unwrap(), "run-[2J1 → failed\na\tb");
    }

    #[test]
    fn json_output_preserves_controls_in_keys_and_values() {
        let controls: String = (0..=0x9f).filter_map(char::from_u32).collect();
        let value = serde_json::json!({
            "ab": 1,
            "a\u{85}b": 2,
            controls.clone(): format!("{controls}\\n échantillon → ok"),
        });
        for serialized in
            [serde_json::to_string(&value).unwrap(), serde_json::to_string_pretty(&value).unwrap()]
        {
            let escaped = terminal_json(&serialized);
            assert_eq!(serde_json::from_str::<serde_json::Value>(&escaped).unwrap(), value);
            assert!(escaped.chars().all(|c| !c.is_control() || c == '\n'));
            let mut out = Vec::new();
            write_terminal(&mut out, format_args!("{escaped}"), true);
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&out).unwrap(), value);
        }
    }

    #[test]
    fn output_macros_use_the_right_stream_and_newlines() {
        const CHILD: &str = "BIFFIX_TEST_OUTPUT_MACROS";
        if std::env::var_os(CHILD).is_some() {
            tprint!("out\u{1b}[2J\t");
            tprintln!("line\u{7}");
            teprintln!("err\u{9b}line\r");
            teprint!("prompt\u{7}: ");
            std::process::exit(0);
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "render::tests::output_macros_use_the_right_stream_and_newlines",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        // The child is this test executable, so stdout starts with the harness's header.
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.ends_with("out[2J\tline\n"), "{stdout:?}");
        assert_eq!(out.stderr, b"errline\nprompt: ");
    }

    #[test]
    fn an_explicit_format_forces_its_mode() {
        // The regression the `resolve` doc names: an explicit `--output human` must stay human even
        // when stdout is piped (as it is under the test harness), and `json` stays JSON. `Auto`
        // depends on the terminal, so it is not pinned here.
        assert_eq!(OutputMode::resolve(OutputFormat::Human), OutputMode::Human);
        assert_eq!(OutputMode::resolve(OutputFormat::Json), OutputMode::Json);
    }
}
