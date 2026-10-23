//! Text edits to an author's `biffix.yaml`, for `biffix inspect --write`: add images to its
//! `images:` block and set its `plugins:` list, leaving everything else (comments, key order,
//! formatting) as it is. Each edit is checked to parse and to say what it was meant to.

use biffix_workflow::images::NewImage;
use biffix_workflow::manifest::BiffixManifest;

use crate::error::{CliError, CliResult};

/// `biffix.yaml`'s `text` with `added` joined to `images:` and, when given, `plugins:` set.
pub(crate) fn updated_manifest(
    text: &str,
    added: &[NewImage],
    plugins: Option<&[String]>,
) -> CliResult<String> {
    let mut updated = text.to_string();
    if !added.is_empty() {
        updated = add_to_images_block(&updated, added)?;
    }
    if let Some(p) = plugins {
        updated = set_plugins_block(&updated, p)?;
    }
    Ok(updated)
}

/// A `biffix.yaml`'s lines, each with its line break (the last given one if it had none).
fn yaml_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    if let Some(last) = lines.last_mut() {
        if !last.ends_with('\n') {
            last.push('\n');
        }
    }
    lines
}

/// The line of the top-level `<key>:`, if any.
fn top_level_header(lines: &[String], key: &str) -> Option<usize> {
    lines.iter().position(|l| {
        l.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r', '#']))
    })
}

/// The value written on the `<key>:` header line itself (comment dropped, trimmed).
fn inline_value<'a>(header: &'a str, key: &str) -> &'a str {
    header[key.len() + 1..].split('#').next().unwrap_or_default().trim()
}

/// `text` (a `biffix.yaml`) with `added` appended to its top-level `images:` block — created at
/// the end when there is none. Everything else (comments, key order, formatting) is left as is.
/// The result is checked to parse and to declare every added image.
fn add_to_images_block(text: &str, added: &[NewImage]) -> CliResult<String> {
    let mut lines = yaml_lines(text);
    let is_top_level_key =
        |l: &str| !l.trim().is_empty() && !l.starts_with([' ', '\t', '#']) && !l.starts_with("---");
    let header = top_level_header(&lines, "images");
    let entry = |indent: &str, image: &NewImage| {
        format!(
            "{indent}{}:  # {}\n{indent}{indent}ref: {}\n",
            image.name, image.location, image.reference
        )
    };
    match header {
        Some(i) => {
            match inline_value(&lines[i], "images") {
                "" => {}
                "{}" => lines[i] = "images:\n".into(),
                _ => {
                    return Err(CliError::InvalidInput(
                        "biffix.yaml writes `images:` inline; rewrite it as a block (one image \
                         per line) so `biffix inspect --write` can add to it"
                            .into(),
                    ))
                }
            }
            let end =
                (i + 1..lines.len()).find(|j| is_top_level_key(&lines[*j])).unwrap_or(lines.len());
            let indent = lines[i + 1..end]
                .iter()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .map(|l| l[..l.len() - l.trim_start().len()].to_string())
                .unwrap_or_else(|| "  ".into());
            // Insert after the block's last entry line (indented): blank lines and top-level
            // comments that follow it stay between the block and the next key.
            let at = (i + 1..end)
                .rev()
                .find(|j| lines[*j].starts_with([' ', '\t']) && !lines[*j].trim().is_empty())
                .map_or(i + 1, |j| j + 1);
            let block: String = added.iter().map(|image| entry(&indent, image)).collect();
            lines.insert(at, block);
        }
        None => {
            if !lines.is_empty() {
                lines.push("\n".into());
            }
            lines.push("images:\n".into());
            lines.extend(added.iter().map(|image| entry("  ", image)));
        }
    }
    let updated: String = lines.concat();
    let parsed = BiffixManifest::parse_yaml(&updated).map_err(|e| {
        CliError::Unclassified(format!(
            "adding the images would make biffix.yaml invalid ({e}); add them by hand"
        ))
    })?;
    for image in added {
        let declared = parsed.images.get(&image.name).and_then(|d| d.reference.as_deref());
        if declared != Some(image.reference.as_str()) {
            return Err(CliError::Unclassified(format!(
                "could not add `{}` to biffix.yaml's images:; add it by hand",
                image.name
            )));
        }
    }
    Ok(updated)
}

/// `text` (a `biffix.yaml`) with its top-level `plugins:` set to `plugins`: the existing block
/// (inline or one entry per line) replaced where it is, else one added at the end; no block when
/// `plugins` is empty. Everything else is left as is. The result is checked to parse and to list
/// exactly `plugins`.
fn set_plugins_block(text: &str, plugins: &[String]) -> CliResult<String> {
    let mut lines = yaml_lines(text);
    let block: String = if plugins.is_empty() {
        String::new()
    } else {
        std::iter::once("plugins:\n".to_string())
            .chain(plugins.iter().map(|p| format!("  - {p}\n")))
            .collect()
    };
    match top_level_header(&lines, "plugins") {
        Some(i) => {
            let inline = !inline_value(&lines[i], "plugins").is_empty();
            // A block's entries are indented, or (YAML allows it) `- ` at the key's own column.
            let is_entry = |l: &str| {
                !l.trim().is_empty()
                    && !l.starts_with("---")
                    && (l.starts_with([' ', '\t']) || l.starts_with('-'))
            };
            let is_top_level_key =
                |l: &str| !l.trim().is_empty() && !l.starts_with([' ', '\t', '#', '-']);
            let end = if inline {
                i + 1
            } else {
                let next = (i + 1..lines.len())
                    .find(|j| is_top_level_key(&lines[*j]))
                    .unwrap_or(lines.len());
                // Through the block's last entry: blank lines and top-level comments after it stay.
                (i + 1..next).rev().find(|j| is_entry(&lines[*j])).map_or(i + 1, |j| j + 1)
            };
            lines.splice(i..end, [block]);
        }
        None if plugins.is_empty() => {}
        None => {
            if !lines.is_empty() {
                lines.push("\n".into());
            }
            lines.push(block);
        }
    }
    let updated: String = lines.concat();
    let parsed = BiffixManifest::parse_yaml(&updated).map_err(|e| {
        CliError::Unclassified(format!(
            "setting `plugins:` would make biffix.yaml invalid ({e}); set it by hand"
        ))
    })?;
    if parsed.plugins != plugins {
        return Err(CliError::Unclassified(
            "could not set biffix.yaml's `plugins:`; set it by hand".into(),
        ));
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(name: &str, reference: &str) -> NewImage {
        NewImage { name: name.into(), reference: reference.into(), location: "process X".into() }
    }

    #[test]
    fn new_images_join_the_existing_block_and_nothing_else_moves() {
        let text = "# my workflow\nengine: nextflow   # pinned below\nimages:\n    bwa:\n        ref: quay.io/b/bwa:1  # aligner\n\n# the rest\nengine_version: \"25.10.4\"\n";
        let out = add_to_images_block(text, &[image("fastqc-1", "quay.io/b/fastqc:1")]).unwrap();
        assert_eq!(
            out,
            "# my workflow\nengine: nextflow   # pinned below\nimages:\n    bwa:\n        ref: quay.io/b/bwa:1  # aligner\n    fastqc-1:  # process X\n        ref: quay.io/b/fastqc:1\n\n# the rest\nengine_version: \"25.10.4\"\n"
        );
    }

    #[test]
    fn a_block_is_created_when_there_is_none_or_it_is_empty() {
        let out =
            add_to_images_block("engine: snakemake", &[image("x-1", "quay.io/b/x:1")]).unwrap();
        assert_eq!(
            out,
            "engine: snakemake\n\nimages:\n  x-1:  # process X\n    ref: quay.io/b/x:1\n"
        );
        let out = add_to_images_block("", &[image("x-1", "quay.io/b/x:1")]).unwrap();
        assert_eq!(out, "images:\n  x-1:  # process X\n    ref: quay.io/b/x:1\n");
        let out = add_to_images_block(
            "images: {}\nimage: ubuntu:24.04\n",
            &[image("x-1", "quay.io/b/x:1")],
        )
        .unwrap();
        assert_eq!(
            out,
            "images:\n  x-1:  # process X\n    ref: quay.io/b/x:1\nimage: ubuntu:24.04\n"
        );
    }

    #[test]
    fn an_inline_images_map_is_left_for_the_author() {
        let err = add_to_images_block(
            "images: {bwa: {ref: quay.io/b/bwa:1}}\n",
            &[image("x-1", "quay.io/b/x:1")],
        )
        .unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("writes `images:` inline")),
            "{err:?}"
        );
    }

    fn plugins(list: &[&str]) -> Vec<String> {
        list.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn a_plugins_block_is_added_at_the_end_when_there_is_none() {
        let out =
            set_plugins_block("# wf\nengine: nextflow  # pinned\n", &plugins(&["nf-schema@2.7.2"]))
                .unwrap();
        assert_eq!(out, "# wf\nengine: nextflow  # pinned\n\nplugins:\n  - nf-schema@2.7.2\n");
        assert_eq!(
            set_plugins_block("", &plugins(&["nf-schema@2.7.2"])).unwrap(),
            "plugins:\n  - nf-schema@2.7.2\n"
        );
        // Nothing to list, nothing listed: left alone (but for the final newline).
        assert_eq!(set_plugins_block("engine: nextflow", &[]).unwrap(), "engine: nextflow\n");
    }

    #[test]
    fn an_existing_plugins_block_is_replaced_in_place_and_nothing_else_moves() {
        let want = plugins(&["nf-prov@1.4.0", "nf-schema@2.7.3"]);
        // Indented entries, with a trailing blank line and comment that belong to what follows.
        let text = "engine: nextflow\nplugins:  # pinned\n    - nf-schema@2.7.2\n\n# images\nimages:\n  x:\n    ref: a:1\n";
        assert_eq!(
            set_plugins_block(text, &want).unwrap(),
            "engine: nextflow\nplugins:\n  - nf-prov@1.4.0\n  - nf-schema@2.7.3\n\n# images\nimages:\n  x:\n    ref: a:1\n"
        );
        // Entries at the key's own column.
        let text = "plugins:\n- nf-schema@2.7.2\n- nf-old@1.0.0\nengine: nextflow\n";
        assert_eq!(
            set_plugins_block(text, &want).unwrap(),
            "plugins:\n  - nf-prov@1.4.0\n  - nf-schema@2.7.3\nengine: nextflow\n"
        );
        // Inline.
        let text = "plugins: [nf-schema@2.7.2]\nengine: nextflow\n";
        assert_eq!(
            set_plugins_block(text, &want).unwrap(),
            "plugins:\n  - nf-prov@1.4.0\n  - nf-schema@2.7.3\nengine: nextflow\n"
        );
        // A pipeline that declares none any more: the block goes.
        let text = "plugins:\n  - nf-schema@2.7.2\nengine: nextflow\n";
        assert_eq!(set_plugins_block(text, &[]).unwrap(), "engine: nextflow\n");
    }

    #[test]
    fn images_and_plugins_are_written_in_one_pass() {
        let text = "engine: nextflow\nplugins:\n  - nf-schema@2.7.2\n";
        let out = updated_manifest(
            text,
            &[image("x-1", "quay.io/b/x:1")],
            Some(&plugins(&["nf-schema@2.8.0"])),
        )
        .unwrap();
        assert_eq!(
            out,
            "engine: nextflow\nplugins:\n  - nf-schema@2.8.0\n\nimages:\n  x-1:  # process X\n    ref: quay.io/b/x:1\n"
        );
        // Plugins not read (or unchanged): left as written, images still added.
        let out = updated_manifest(text, &[image("x-1", "quay.io/b/x:1")], None).unwrap();
        assert!(out.starts_with(text), "{out}");
    }
}
