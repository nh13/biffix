//! biffix's repository tooling, run as `cargo xtask <task>` (the alias is in `.cargo/config.toml`).
//!
//!   cargo xtask emit-schemas           # write schemas/*.json
//!   cargo xtask emit-schemas --check   # verify no drift (CI); writes nothing
//!
//! `emit-schemas` writes the versioned artifacts the other languages read, from the Rust that is
//! canonical: JSON Schemas derived from the contract types, the cross-language fixture tables
//! ([`cases`]) the UI and plugin tests replay, and the lists those tests pin themselves to (the
//! failure causes, the ingest limits). [`artifacts::all`] is the one list of them.
//!
//! Its tests also guard the workspace's crate layering (`layering`).

mod artifacts;
mod cases;
#[cfg(test)]
mod layering;

const USAGE: &str = "usage: cargo xtask emit-schemas [--check]";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Task {
    Help,
    EmitSchemas { check: bool },
}

/// The task `args` (the arguments after the program's name) ask for, or the usage error to print.
fn parse(args: impl IntoIterator<Item = String>) -> Result<Task, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("emit-schemas") => {}
        Some("-h" | "--help" | "help") => return Ok(Task::Help),
        Some(other) => return Err(format!("xtask: unknown task '{other}' ({USAGE})")),
        None => return Err(format!("xtask: no task given ({USAGE})")),
    }
    // Explicitly validate args: an unrecognized/misspelled flag must fail, not silently fall
    // through to write-mode (which would let a typo'd `--chekc` in CI regenerate instead of verify).
    let mut check = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            "-h" | "--help" => return Ok(Task::Help),
            other => {
                return Err(format!(
                    "xtask emit-schemas: unrecognized argument '{other}' ({USAGE})"
                ));
            }
        }
    }
    Ok(Task::EmitSchemas { check })
}

fn main() -> std::io::Result<()> {
    match parse(std::env::args().skip(1)) {
        Ok(Task::Help) => {
            println!("{USAGE}");
            Ok(())
        }
        Ok(Task::EmitSchemas { check }) => emit_schemas(check),
        Err(usage) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}

/// Write every artifact to `schemas/`, or with `check` compare them and exit 1 on drift.
fn emit_schemas(check: bool) -> std::io::Result<()> {
    let dir = artifacts::schemas_dir();
    let all = artifacts::all();

    if check {
        let drift = artifacts::check(&dir, &all)?;
        for d in &drift {
            eprintln!("DRIFT: {d}");
        }
        if !drift.is_empty() {
            std::process::exit(1);
        }
        return Ok(());
    }

    std::fs::create_dir_all(&dir).map_err(artifacts::at(&dir))?;
    for (name, content) in &all {
        let path = dir.join(name);
        std::fs::write(&path, content).map_err(artifacts::at(&path))?;
        println!("wrote schemas/{name}");
    }
    // Not deleted automatically (the directory is checked in; removing a file is a reviewable
    // change), but `--check` fails until it is.
    for name in artifacts::stale_files(&dir, &all)? {
        eprintln!("warning: schemas/{name} is no longer emitted; delete it");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Task, String> {
        parse(args.iter().map(ToString::to_string))
    }

    #[test]
    fn the_command_line_names_the_task() {
        assert_eq!(parsed(&["emit-schemas"]), Ok(Task::EmitSchemas { check: false }));
        assert_eq!(parsed(&["emit-schemas", "--check"]), Ok(Task::EmitSchemas { check: true }));
        for help in [&["--help"][..], &["-h"], &["help"], &["emit-schemas", "--help"]] {
            assert_eq!(parsed(help), Ok(Task::Help), "{help:?}");
        }
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        // A misspelt flag must never fall through to writing the schemas.
        for args in [&[][..], &["emit"], &["emit-schemas", "--chekc"]] {
            let err = parsed(args).unwrap_err();
            assert!(err.ends_with(&format!("({USAGE})")), "{args:?}: {err}");
        }
    }
}
