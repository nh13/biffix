//! The register-time help lint: the launch-form params that explain nothing.

use crate::formschema::{FormField, FormSchema};

/// How many param names [`help_lint`] lists before counting the rest.
const LINT_SHOWN: usize = 8;

/// The params a biologist meets on the launch form that explain nothing: no description and no
/// help. A param counts when it is shown without *Show all parameters* (not hidden), and either is
/// required or sits in a declared section that is not advanced. Params the reference dropdown
/// fills are left out (the dropdown names what it provides).
pub(super) fn undocumented_params(form: &FormSchema) -> Vec<&FormField> {
    let shown_sections: std::collections::HashSet<&str> =
        form.sections.iter().filter(|s| !s.advanced).map(|s| s.id.as_str()).collect();
    let filled = |name: &str| form.references.as_ref().is_some_and(|r| r.fill.contains_key(name));
    let said = |t: &Option<String>| t.as_deref().is_some_and(|t| !t.trim().is_empty());
    form.fields
        .iter()
        .filter(|f| !f.hidden && !filled(&f.name))
        .filter(|f| f.required || f.section.as_deref().is_some_and(|s| shown_sections.contains(s)))
        .filter(|f| !said(&f.description) && !said(&f.help))
        .collect()
}

/// The registration warning for the form's shown parameters with neither a description nor a
/// help (required ones, and those in a section not marked advanced, that a reference does not fill
/// and that are not hidden), if any: a lint, never an error.
pub fn help_lint(form: &FormSchema) -> Option<String> {
    let missing = undocumented_params(form);
    if missing.is_empty() {
        return None;
    }
    let mut names: Vec<String> = missing.iter().take(LINT_SHOWN).map(|f| f.name.clone()).collect();
    if missing.len() > LINT_SHOWN {
        names.push(format!("and {} more", missing.len() - LINT_SHOWN));
    }
    Some(format!(
        "{} parameter(s) the launch form shows have no description or help: {}. A biologist sees \
         only the name; add a `description` (or `help_text`) in the schema, or `help:` under \
         `params.<name>` in biffix.yaml",
        missing.len(),
        names.join(", ")
    ))
}
