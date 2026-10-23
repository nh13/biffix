//! `biffix workflows list|release|unrelease|show` and `biffix engines`: what is registered, and
//! where each version's release stands.

use biffix_workflow::formschema::FormSchema;
use serde::Deserialize;

use crate::commands::Session;
use crate::error::{CliError, CliResult};
use crate::render::OutputMode;

/// `biffix workflows list`: every registered version, or (`released`) only the released
/// (`Some(true)`) or only the developer (`Some(false)`) ones.
pub async fn list(session: &mut Session, released: Option<bool>) -> CliResult<()> {
    let client = session.authed_client().await?;
    let workflows = client.list_workflows_released(released).await?;
    match session.output {
        // The API's own shape (readiness, images, provenance …), as `runs list` prints its runs.
        OutputMode::Json => crate::render::print_json(&workflows),
        OutputMode::Human => {
            if workflows.is_empty() {
                println!("no registered workflows");
            }
            for w in &workflows {
                let kind = release_word(&w.release);
                println!("{}  {}  {} {}  {kind}", w.id, w.name, w.engine, w.engine_version);
            }
        }
    }
    Ok(())
}

/// `biffix workflows release` / `unrelease`: release a version (the Lab offers released versions
/// by default and launches a pipeline's newest released one), or take the release back.
pub async fn release(session: &mut Session, id: &str, released: bool) -> CliResult<()> {
    let id = version_id(id)?;
    let client = session.authed_client().await?;
    let release = client.set_workflow_released(id, released).await?;
    match session.output {
        OutputMode::Json => crate::render::print_json(&release),
        OutputMode::Human => println!("{}", release_line(&release)),
    }
    Ok(())
}

/// A version id as given, trimmed; a blank one is refused before any request (`GET /workflows/`
/// with no id is the list route, not a version).
fn version_id(id: &str) -> CliResult<&str> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CliError::InvalidInput("a version id must not be blank".into()));
    }
    Ok(id)
}

/// One line saying where a version's release stands: released (by whom), or a developer version.
fn release_line(r: &biffix_api::WorkflowRelease) -> String {
    format!("{}  {}", r.version_id, release_state(&r.release))
}

/// A version's release in a word: `released` or `developer`.
fn release_word(r: &biffix_api::VersionRelease) -> &'static str {
    if r.released {
        "released"
    } else {
        "developer"
    }
}

/// Where a version's release stands: `released by <who>`, or `developer (not released)`.
fn release_state(r: &biffix_api::VersionRelease) -> String {
    if r.released {
        let by = r
            .by_label
            .as_deref()
            .or(r.by.as_deref())
            .map(|who| format!(" by {who}"))
            .unwrap_or_default();
        format!("released{by}")
    } else {
        "developer (not released)".to_string()
    }
}

/// `biffix engines`: the engines the deployment runs and the versions a workflow may pin.
pub(crate) async fn engines(session: &mut Session) -> CliResult<()> {
    let client = session.authed_client().await?;
    let engines = client.list_engines().await?;
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!(engines)),
        OutputMode::Human => {
            for e in &engines {
                match e.releases.last() {
                    None => println!(
                        "{}  >={}, <{}  (pin an exact release, e.g. engine_version: \"{}\")",
                        e.engine, e.min_version, e.below_version, e.min_version
                    ),
                    // Only these releases can build (their downloads are pinned).
                    Some(latest) => println!(
                        "{}  one of {}  (e.g. engine_version: \"{latest}\")",
                        e.engine,
                        e.releases.join(", "),
                    ),
                }
            }
        }
    }
    Ok(())
}

pub async fn show(session: &mut Session, id: &str) -> CliResult<()> {
    let id = version_id(id)?;
    let client = session.authed_client().await?;
    let detail = client.get_workflow(id).await?;
    match session.output {
        // The full `GET /workflows/{id}` detail, not a subset that drops fields as the API grows.
        OutputMode::Json => crate::render::print_json(&detail),
        OutputMode::Human => {
            for line in show_lines(&detail) {
                println!("{line}");
            }
        }
    }
    Ok(())
}

/// `workflows show`'s human lines. A form this CLI cannot read is said to be so, not left out as
/// if the version had no parameters.
fn show_lines(detail: &biffix_api::WorkflowVersionDetail) -> Vec<String> {
    let mut lines = vec![format!("{} ({})", detail.name, detail.id)];
    if !detail.image_digest.is_empty() {
        lines.push(format!("  image:    {}", detail.image_digest));
    }
    lines.push(format!("  engine:   {} {}", detail.engine, detail.engine_version));
    lines.push(format!("  entry:    {}", detail.entrypoint));
    lines.push(format!("  release:  {}", release_state(&detail.release)));
    match FormSchema::deserialize(&detail.form_schema) {
        Ok(fs) => {
            lines.push("  params:".into());
            for f in &fs.fields {
                let req = if f.required { " (required)" } else { "" };
                lines.push(format!("    - {}{req}", f.name));
            }
        }
        Err(e) => lines.push(format!("  params:   (form unreadable by this CLI: {e})")),
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_says_when_it_cannot_read_the_form() {
        let mut json = serde_json::json!({
            "id": "a:h:0", "name": "wf", "project": "p", "image_digest": "",
            "engine": "snakemake", "engine_version": "9.27.0", "git_sha": "a",
            "queue_classes": [], "created_at_micros": 0, "source_content_hash": "h",
            "entrypoint": "Snakefile",
            "form_schema": {"fields": [{"name": "reads", "widget": "s3_file", "required": true,
                "is_output": false, "is_secret": false, "is_path_input": true}], "warnings": []},
            "source": {"kind": "git", "repo": "r", "git_ref": "main"}, "build": 0,
            "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        });
        let detail = |json: &serde_json::Value| -> biffix_api::WorkflowVersionDetail {
            serde_json::from_value(json.clone()).unwrap()
        };
        let lines = show_lines(&detail(&json));
        assert!(lines.contains(&"    - reads (required)".to_string()), "{lines:?}");
        json["form_schema"] = serde_json::json!({"fields": "not a list"});
        let lines = show_lines(&detail(&json));
        assert!(
            lines.iter().any(|l| l.starts_with("  params:   (form unreadable by this CLI: ")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_release_line_says_who_released_it_or_that_it_is_a_developer_version() {
        use biffix_api::{WorkflowRelease, WorkflowReleaseChange};
        let change = |by: Option<&str>, by_label: Option<&str>| WorkflowReleaseChange {
            released: true,
            at_micros: 1,
            by: by.map(str::to_string),
            by_label: by_label.map(str::to_string),
        };
        let line = |history| release_line(&WorkflowRelease::of(&"a:h:0".into(), history, true));
        assert_eq!(
            line(vec![change(Some("u1"), Some("u1@lab.example.org"))]),
            "a:h:0  released by u1@lab.example.org"
        );
        assert_eq!(line(vec![change(Some("u1"), None)]), "a:h:0  released by u1");
        assert_eq!(line(vec![change(None, None)]), "a:h:0  released");
        let mut unreleased = change(Some("u1"), None);
        unreleased.released = false;
        assert_eq!(line(vec![unreleased, change(None, None)]), "a:h:0  developer (not released)");
        assert_eq!(line(vec![]), "a:h:0  developer (not released)");
    }
}
