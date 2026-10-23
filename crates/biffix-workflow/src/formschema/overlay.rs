//! The author's `form:` overlay applied to a derived form: presets, sections, `show_if` rules,
//! reference bundles and examples.

use serde_json::Value;

use crate::about::help_description;
use crate::formschema::fields::{checkable_pattern, nonblank};
use crate::formschema::{
    humanize, value_key, FormExample, FormField, FormReferences, FormSchema, FormSection, Preset,
    Rule, ShowIf, WidgetHint,
};
use crate::manifest::FormOverlay;

/// Apply the author's `form:` overlay to a derived form. Overlay values win
/// per key; anything that does not fit the workflow (an unknown param or section, a preset outside
/// a field's choices, a `show_if` on an unknown param, a cycle, an unreadable example file) is a
/// warning, never an error. `read_file` reads a file from the workflow repo (an example's
/// `params_file`).
pub fn apply_overlay(
    mut form: FormSchema,
    overlay: &FormOverlay,
    read_file: &dyn Fn(&str) -> Result<String, String>,
) -> FormSchema {
    form.curated = true;
    // Text is frozen trimmed, blank as none, as every reader of it reads it.
    form.title = nonblank(overlay.title.as_deref()).or(form.title);
    form.description = nonblank(overlay.description.as_deref()).or(form.description);
    form.run_name = nonblank(overlay.run_name.as_deref()).or(form.run_name);

    // Sections: the overlay's, in its order (merged with a derived section of the same id), then
    // the derived sections it does not mention, in their order.
    let mut sections: Vec<FormSection> = Vec::new();
    for so in &overlay.sections {
        let derived = form.sections.iter().find(|s| s.id == so.id);
        sections.push(FormSection {
            id: so.id.clone(),
            title: so
                .title
                .clone()
                .or_else(|| derived.map(|d| d.title.clone()))
                .unwrap_or_else(|| humanize(&so.id)),
            description: so
                .description
                .clone()
                .or_else(|| derived.and_then(|d| d.description.clone())),
            advanced: so.advanced.or_else(|| derived.map(|d| d.advanced)).unwrap_or(false),
        });
    }
    for derived in &form.sections {
        if !sections.iter().any(|s| s.id == derived.id) {
            sections.push(derived.clone());
        }
    }

    for (name, po) in &overlay.params {
        let Some(field) = form.fields.iter_mut().find(|f| &f.name == name) else {
            form.warnings
                .push(format!("form.params.{name}: not a parameter of this workflow; ignored"));
            continue;
        };
        if let Some(section) = &po.section {
            if !sections.iter().any(|s| &s.id == section) {
                form.warnings.push(format!(
                    "form.params.{name}: section `{section}` is not declared; added"
                ));
                sections.push(FormSection {
                    id: section.clone(),
                    title: humanize(section),
                    description: None,
                    advanced: false,
                });
            }
            field.section = Some(section.clone());
        }
        field.label = nonblank(po.label.as_deref()).or(field.label.take());
        // `form.params.<name>.help`/`unit` read as `params.<name>.help`/`unit` do (trimmed, blank
        // is absent) and win over them. A description derived from the help it replaces (or none)
        // is derived again from the new help; one the schema gave is kept.
        if let Some(help) = nonblank(po.help.as_deref()) {
            let derived = field.help.as_deref().and_then(help_description);
            if field.description.is_none() || field.description == derived {
                field.description = help_description(&help);
            }
            field.help = Some(help);
        }
        field.hidden = po.hidden.unwrap_or(field.hidden);
        // The overlay's `default` is the form's prefill, never the engine's default: `show_if`
        // gating on an omitted param must follow what the engine will actually run with.
        if let Some(prefill) = po.default.clone().filter(|v| !v.is_null()) {
            if fits_choices(field, &prefill) {
                field.prefill = Some(prefill);
            } else {
                form.warnings.push(format!(
                    "form.params.{name}: default `{}` is not one of the choices; ignored",
                    value_key(&prefill)
                ));
            }
        }
        field.placeholder = nonblank(po.placeholder.as_deref()).or(field.placeholder.take());
        field.unit = nonblank(po.unit.as_deref()).or(field.unit.take());
        field.multiline = po.multiline.unwrap_or(field.multiline);
        if field.widget == WidgetHint::Number {
            field.min = po.min.or(field.min);
            field.max = po.max.or(field.max);
            field.step = po.step.or(field.step);
        } else {
            for (key, set) in [("min", po.min), ("max", po.max), ("step", po.step)] {
                if set.is_some() {
                    form.warnings.push(format!(
                        "form.params.{name}: `{key}` applies only to a number; ignored"
                    ));
                }
            }
        }
        if let (Some(lo), Some(hi)) = (field.min, field.max) {
            if lo > hi {
                form.warnings.push(format!(
                    "form.params.{name}: min {lo} is above max {hi}; no value can be given"
                ));
            }
        }
        for rule in &po.rules {
            if !checkable_pattern(&format!("form.params.{name}"), &rule.pattern, &mut form.warnings)
            {
                continue;
            }
            field.rules.push(Rule {
                pattern: rule.pattern.clone(),
                message: rule
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("must match {}", rule.pattern)),
            });
        }
        if let Some(presets) = &po.presets {
            field.presets.clear();
            for (label, value) in presets {
                let label = match label.as_str() {
                    Some(l) => l.to_string(),
                    None => yaml_to_json(label)
                        .map(|l| value_key(&l))
                        .unwrap_or_else(|| format!("{label:?}")),
                };
                let Some(value) = yaml_to_json(value) else {
                    form.warnings.push(format!(
                        "form.params.{name}: preset `{label}` is not a plain value; ignored"
                    ));
                    continue;
                };
                if fits_choices(field, &value) {
                    field.presets.push(Preset { label, value });
                } else {
                    form.warnings.push(format!(
                        "form.params.{name}: preset `{label}` is not one of the choices; ignored"
                    ));
                }
            }
        }
        // A samplesheet spec only means something on a samplesheet param: the form draws its table
        // for no other widget, so the API must not take rows for one either.
        if po.samplesheet.is_some() && field.widget != WidgetHint::Samplesheet {
            form.warnings.push(format!(
                "form.params.{name}: samplesheet applies only to a samplesheet param (add \
                 `format: samplesheet`); ignored"
            ));
        } else if let Some(sheet) = &po.samplesheet {
            // Merged onto a copy, so an overlay that is ignored leaves the derived spec as it was.
            let mut spec = field.samplesheet.clone().unwrap_or_default();
            if let Some(columns) = &sheet.columns {
                spec.columns = columns
                    .iter()
                    .map(|c| c.column.clone())
                    .map(|mut c| {
                        c.role = c.role.or_else(|| crate::samples::role_from_name(&c.name));
                        c
                    })
                    .collect();
            }
            spec.format = sheet.format.unwrap_or(spec.format);
            spec.paired_only = sheet.paired_only.unwrap_or(spec.paired_only);
            spec.lanes = sheet.lanes.unwrap_or(spec.lanes);
            if spec.columns.is_empty() {
                form.warnings
                    .push(format!("form.params.{name}: samplesheet declares no columns; ignored"));
            } else {
                field.samplesheet = Some(spec);
            }
        }
        if let Some(conditions) = &po.show_if {
            field.show_if.clear();
            for (other, want) in conditions {
                // A key YAML read as a number or boolean (`1:`) still names a param by its
                // spelling; an unknown name is warned about below.
                let other = match other.as_str() {
                    Some(o) => o.to_string(),
                    None => match yaml_to_json(other) {
                        Some(key @ (Value::Number(_) | Value::Bool(_))) => value_key(&key),
                        _ => {
                            form.warnings.push(format!(
                                "form.params.{name}: show_if key {other:?} is not a parameter \
                                 name; ignored"
                            ));
                            continue;
                        }
                    },
                };
                let values = match yaml_to_json(want) {
                    Some(Value::Array(items)) => items,
                    Some(single) => vec![single],
                    None => {
                        form.warnings.push(format!(
                            "form.params.{name}: show_if value for `{other}` is not a plain \
                             value; ignored"
                        ));
                        continue;
                    }
                };
                field.show_if.push(ShowIf { field: other, values });
            }
        }
    }

    // A `show_if` must name a real param; a condition that never can resolve would hide the field
    // forever, so drop it (the field is then always shown).
    let names: std::collections::BTreeSet<String> =
        form.fields.iter().map(|f| f.name.clone()).collect();
    let mut dropped = Vec::new();
    for field in &mut form.fields {
        field.show_if.retain(|c| {
            let known = names.contains(&c.field) && c.field != field.name;
            if !known {
                dropped.push(format!(
                    "form.params.{}: show_if names `{}`, which is not another parameter; ignored",
                    field.name, c.field
                ));
            }
            known
        });
    }
    form.warnings.extend(dropped);
    // In field order, so the warnings read like the form.
    let cycles = crate::validate::show_if_cycles(&form.fields);
    for f in form.fields.iter().filter(|f| cycles.contains(&f.name)) {
        form.warnings.push(format!(
            "form.params.{}: show_if conditions form a cycle; the field is always shown",
            f.name
        ));
    }

    if let Some(refs) = &overlay.references {
        form.references =
            references_from(refs, &mut form.fields, &mut sections, &mut form.warnings);
    }

    // Order fields by their section, keeping source order within one; drop empty sections.
    let rank = |f: &FormField| {
        f.section
            .as_ref()
            .and_then(|id| sections.iter().position(|s| &s.id == id))
            .unwrap_or(usize::MAX)
    };
    form.fields.sort_by_key(rank);
    sections.retain(|s| form.fields.iter().any(|f| f.section.as_ref() == Some(&s.id)));
    form.sections = sections;

    for ex in &overlay.examples {
        form.examples.push(example_from(ex, &form.fields, read_file, &mut form.warnings));
    }
    form
}

/// Whether `value` is one of `field`'s choices (any value fits a field without choices).
fn fits_choices(field: &FormField, value: &Value) -> bool {
    field.enum_values.as_ref().is_none_or(|allowed| allowed.contains(&value_key(value)))
}

/// The form's reference dropdown from the author's `references:` block. A `fill` target that is not
/// a param is a warning and dropped; filled params move into the dropdown's section so *Use my own
/// files…* shows them in its card. `None` when nothing is left to fill.
fn references_from(
    refs: &crate::manifest::ReferencesOverlay,
    fields: &mut [FormField],
    sections: &mut Vec<FormSection>,
    warnings: &mut Vec<String>,
) -> Option<FormReferences> {
    let mut fill = std::collections::BTreeMap::new();
    for (param, key) in &refs.fill {
        match fields.iter_mut().find(|f| &f.name == param) {
            Some(field) => {
                if let Some(section) = &refs.section {
                    field.section = Some(section.clone());
                }
                fill.insert(param.clone(), key.clone());
            }
            None => warnings.push(format!(
                "form.references.fill.{param}: not a parameter of this workflow; ignored"
            )),
        }
    }
    if fill.is_empty() {
        warnings.push("form.references: fills no parameter; ignored".into());
        return None;
    }
    let keys: std::collections::BTreeSet<&String> = fill.values().collect();
    let required = match &refs.required {
        Some(required) => {
            for key in required.iter().filter(|k| !keys.contains(k)) {
                warnings.push(format!(
                    "form.references.required: `{key}` fills no parameter; it only narrows the bundles offered"
                ));
            }
            required.clone()
        }
        None => keys.iter().map(|k| (*k).clone()).collect(),
    };
    if let Some(section) = &refs.section {
        if !sections.iter().any(|s| &s.id == section) {
            sections.push(FormSection {
                id: section.clone(),
                title: refs.label.clone().unwrap_or_else(|| humanize(section)),
                description: None,
                advanced: false,
            });
        }
    }
    Some(FormReferences {
        kind: refs.kind.clone(),
        fill,
        required,
        label: refs.label.clone().unwrap_or_else(|| "Reference genome".to_string()),
        section: refs.section.clone(),
        custom: refs.custom.unwrap_or(true),
        help: nonblank(refs.help.as_deref()),
    })
}

/// A YAML value as JSON; `None` for one JSON cannot hold (a tagged value, a mapping with a key
/// that is not a plain value), which the caller warns about.
fn yaml_to_json(value: &serde_norway::Value) -> Option<Value> {
    serde_norway::from_value(value.clone()).ok()
}

fn example_from(
    ex: &crate::manifest::ExampleOverlay,
    fields: &[FormField],
    read_file: &dyn Fn(&str) -> Result<String, String>,
    warnings: &mut Vec<String>,
) -> FormExample {
    let mut params = serde_json::Map::new();
    if let Some(path) = &ex.params_file {
        let parsed = read_file(path).and_then(|text| {
            serde_norway::from_str::<serde_json::Map<String, Value>>(&text)
                .map_err(|e| e.to_string())
        });
        match parsed {
            Ok(map) => params.extend(map),
            Err(e) => {
                warnings
                    .push(format!("example `{}`: cannot read params_file {path}: {e}", ex.name));
            }
        }
    }
    if let Some(inline) = &ex.params {
        params.extend(inline.clone());
    }
    params.retain(|name, _| {
        let known = fields.iter().any(|f| &f.name == name);
        if !known {
            warnings.push(format!(
                "example `{}`: `{name}` is not a parameter of this workflow; ignored",
                ex.name
            ));
        }
        known
    });
    let samplesheet_uri = ex.samplesheet.clone().filter(|uri| {
        let ok = uri.starts_with("s3://");
        if !ok {
            warnings
                .push(format!("example `{}`: samplesheet must be an s3:// URI; ignored", ex.name));
        }
        ok
    });
    FormExample {
        name: ex.name.clone(),
        description: ex.description.clone(),
        params,
        samplesheet_uri,
        reference: ex.reference.clone(),
        run_name: ex.run_name.clone(),
    }
}
