//! `biffix.yaml` — the author↔platform contract, and engine
//! detection.
//!
//! Lives in biffix-workflow so the CLI and the backend derive the manifest contract from one
//! definition.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::engines::Engine;

/// The manifest's `engine:` — an [`Engine`], or `auto` (detect it from the repo's files, as when
/// the key is absent). Only the manifest has `auto`; [`detect_engine`] resolves it to an
/// [`Engine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineChoice {
    Auto,
    Engine(Engine),
}

impl<'de> Deserialize<'de> for EngineChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        if name == "auto" {
            return Ok(EngineChoice::Auto);
        }
        name.parse::<Engine>().map(EngineChoice::Engine).map_err(|_| {
            serde::de::Error::custom(format!(
                "unknown engine `{name}`, expected one of `snakemake`, `nextflow`, `auto`"
            ))
        })
    }
}

/// A per-param widget/semantic hint the engine schema can't express.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ParamHint {
    /// The widget: `s3-file`, `s3-dir`, `reference-bundle` or `samplesheet`. A secret is
    /// `secret: true`, not a format.
    #[serde(default)]
    pub format: Option<String>,
    /// File-name ending(s) a path accepts, comma-separated: `.fastq.gz, .fq.gz`.
    #[serde(default)]
    pub suffix: Option<String>,
    /// The param takes a list of paths.
    #[serde(default)]
    pub multiple: bool,
    /// A path the workflow writes to: never existence-checked.
    #[serde(default)]
    pub output: bool,
    /// Stored encrypted and never echoed.
    #[serde(default)]
    pub secret: bool,
    /// A help paragraph for the launch form's ⓘ (what the param does, when to change it), over
    /// the schema's `help_text`. Under `form.params` the overlay's own `help` reads the same key.
    #[serde(default)]
    pub help: Option<String>,
    /// A unit shown with the label (`bp`, `reads`), over the schema's own `unit`/`x-unit`. Under
    /// `form.params` the overlay's own `unit` reads the same key.
    #[serde(default)]
    pub unit: Option<String>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// The parsed manifest. Optional fields fall back to detection.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct BiffixManifest {
    #[serde(default)]
    pub engine: Option<EngineChoice>,
    #[serde(default)]
    pub engine_version: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamHint>,
    /// The author's curation of the launch form. Applied over the form the
    /// adapters derive from the engine schema; absent ⇒ the derived form as-is.
    #[serde(default)]
    pub form: Option<FormOverlay>,
    /// A repo-relative Markdown file shown as the workflow's About page. Only
    /// this file: a README is often developer docs, so it is never picked up by default.
    #[serde(default)]
    pub about: Option<String>,
    /// Reports a run writes that its page should show: a name and a path or glob
    /// relative to the run's output prefix.
    #[serde(default)]
    pub reports: Vec<ReportDecl>,
    /// How to match the workflow's tasks to samples for per-sample progress, when the default
    /// rules (a `sample` wildcard, a Nextflow tag naming a sample) do not fit.
    #[serde(default)]
    pub samples: Option<SampleDecl>,
    /// How the workflow reads in the Lab's pipeline catalog: a plain description, tags, and
    /// whether the author vouches for it (see [`crate::formschema::attach_catalog`]).
    #[serde(default)]
    pub catalog: Option<CatalogDecl>,
    /// Snakemake only: reuse the results of the workflow's `cache: True` rules across runs (the
    /// engine's between-workflow output cache, kept per project in S3).
    #[serde(default)]
    pub cache: bool,
    /// The default image, as a reference: sugar for `images: {default: {ref: …}}`.
    #[serde(default)]
    pub image: Option<String>,
    /// The workflow's container images by name (`default` is the one tasks without their own run
    /// on); see [`crate::images`].
    #[serde(default)]
    pub images: BTreeMap<String, crate::images::ImageDecl>,
    /// Nextflow only: the plugins the workflow's own `nextflow.config` declares, each `id@version`
    /// (`nf-schema@2.7.2`). The coordinator runs Nextflow offline, so registration pins each by
    /// sha256 and builds it into the runtime layer; `biffix inspect --write` fills this in.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

impl BiffixManifest {
    /// The images to import: `images:` plus `image:` (validated, sorted by name).
    pub fn declared_images(
        &self,
    ) -> Result<Vec<crate::images::DeclaredImage>, crate::images::ImageDeclError> {
        crate::images::declared_images(&self.images, self.image.as_deref())
    }

    /// The workflow's `plugins:` as `(id, version)`, sorted by id and checked
    /// ([`crate::runtime::parse_plugin_specs`]). Only a Nextflow workflow may declare any.
    pub fn nextflow_plugins(
        &self,
        engine: Engine,
    ) -> Result<Vec<(String, String)>, crate::runtime::WorkflowPluginError> {
        if !self.plugins.is_empty() && engine != Engine::Nextflow {
            return Err(crate::runtime::WorkflowPluginError(
                "biffix.yaml `plugins:` applies only to Nextflow workflows".into(),
            ));
        }
        crate::runtime::parse_plugin_specs(&self.plugins)
    }

    /// Parse `biffix.yaml`. A `form.params` entry may carry the same widget hints as a top-level
    /// `params` entry (`format`, `secret`, …); they are folded into `params` here so the adapters'
    /// one widget picker sees them. A `format` or `suffix` under `form.params` replaces the
    /// top-level one; a boolean hint (`multiple`, `output`, `secret`) set in either place holds,
    /// since an unset boolean cannot be told from `false`.
    pub fn parse_yaml(text: &str) -> Result<Self, ManifestError> {
        let mut manifest: Self =
            serde_norway::from_str(text).map_err(|e| ManifestError(e.to_string()))?;
        if let Some(form) = &manifest.form {
            for (name, overlay) in &form.params {
                let hint = manifest.params.entry(name.clone()).or_default();
                let from = &overlay.hint;
                if from.format.is_some() {
                    hint.format = from.format.clone();
                }
                if from.suffix.is_some() {
                    hint.suffix = from.suffix.clone();
                }
                hint.multiple |= from.multiple;
                hint.output |= from.output;
                hint.secret |= from.secret;
            }
        }
        Ok(manifest)
    }

    /// Every key of the manifest that biffix does not know (a typo such as `show-if`, or a key
    /// from another tool), as one registration warning each: serde would otherwise drop them
    /// without a word.
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut note = |at: &str, extra: &serde_norway::Mapping| {
            for key in extra.keys() {
                let key = match key.as_str() {
                    Some(k) => k.to_string(),
                    None => serde_norway::to_string(key)
                        .map(|k| k.trim().to_string())
                        .unwrap_or_default(),
                };
                out.push(format!("biffix.yaml: {at}unknown key `{key}`; ignored"));
            }
        };
        note("", &self.extra);
        for (name, hint) in &self.params {
            note(&format!("params.{name}: "), &hint.extra);
        }
        for report in &self.reports {
            note(&format!("reports `{}`: ", report.name), &report.extra);
        }
        if let Some(samples) = &self.samples {
            note("samples: ", &samples.extra);
        }
        if let Some(catalog) = &self.catalog {
            note("catalog: ", &catalog.extra);
            if let Some(contact) = &catalog.contact {
                note("catalog.contact: ", &contact.extra);
            }
        }
        if let Some(form) = &self.form {
            note("form: ", &form.extra);
            for section in &form.sections {
                note(&format!("form.sections `{}`: ", section.id), &section.extra);
            }
            for (name, param) in &form.params {
                note(&format!("form.params.{name}: "), &param.hint.extra);
                for rule in &param.rules {
                    note(&format!("form.params.{name}.rules: "), &rule.extra);
                }
                if let Some(sheet) = &param.samplesheet {
                    note(&format!("form.params.{name}.samplesheet: "), &sheet.extra);
                    for column in sheet.columns.iter().flatten() {
                        note(
                            &format!(
                                "form.params.{name}.samplesheet.columns `{}`: ",
                                column.column.name
                            ),
                            &column.extra,
                        );
                    }
                }
            }
            for example in &form.examples {
                note(&format!("form.examples `{}`: ", example.name), &example.extra);
            }
            if let Some(refs) = &form.references {
                note("form.references: ", &refs.extra);
            }
        }
        out
    }
}

/// The `catalog:` block of `biffix.yaml`: how the workflow is listed in the Lab's pipeline
/// catalog. Author-owned and versioned with the workflow.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct CatalogDecl {
    /// A plain-language description for the pipeline's card (else the form's description).
    #[serde(default)]
    pub description: Option<String>,
    /// Free-form tags the catalog filters on (`RNA-seq`, `Bulk`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// The author vouches for the workflow (a *Verified* badge).
    #[serde(default)]
    pub verified: bool,
    /// Who Lab users ask when one of the workflow's runs needs attention (wins over the site's
    /// support contact).
    #[serde(default)]
    pub contact: Option<ContactDecl>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// `catalog.contact` in `biffix.yaml`: `{name, email, url?}`. Every key is optional here so a
/// missing one is a registration warning (see [`crate::formschema::normalize_contact`]), not a
/// parse error.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ContactDecl {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// One report a workflow's runs write (`reports:` in `biffix.yaml`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ReportDecl {
    pub name: String,
    /// A path or glob (`*`, `**`, `?`) relative to the run's output prefix. A `per_sample` report's
    /// path holds `{sample}`.
    pub path: String,
    #[serde(default)]
    pub description: Option<String>,
    /// How the report is shown: `html`, `table`, `image`, `pdf`, `alignment`, `reads`, `fastqc` or
    /// `file` (see [`crate::formschema::REPORT_KINDS`]); absent ⇒ from the file's name.
    #[serde(default)]
    pub kind: Option<String>,
    /// One report per sample: `{sample}` in the path is each of the run's sample ids in turn.
    #[serde(default)]
    pub per_sample: bool,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// The `form:` block of `biffix.yaml`: how the author wants the launch form to read. Every key is
/// optional; params are keyed by the engine schema's param names.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct FormOverlay {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Sections in display order; derived sections the overlay does not list follow in their own
    /// order.
    #[serde(default)]
    pub sections: Vec<SectionOverlay>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamOverlay>,
    #[serde(default)]
    pub examples: Vec<ExampleOverlay>,
    /// A run-name template, e.g. `"rnaseq · {date}"`.
    #[serde(default)]
    pub run_name: Option<String>,
    /// One reference-bundle dropdown that fills several params.
    #[serde(default)]
    pub references: Option<ReferencesOverlay>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// The author's reference-bundle declaration: which params a chosen bundle fills, from which of
/// its keys, and which keys a bundle must have to be offered.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ReferencesOverlay {
    /// Offer only bundles of this kind (`assembly`, `assay_kit`).
    #[serde(default)]
    pub kind: Option<String>,
    /// Param name → bundle key (`ref_fasta: fasta`).
    #[serde(default)]
    pub fill: BTreeMap<String, String>,
    /// Bundle keys a bundle must have to be offered; defaults to every `fill` key.
    #[serde(default)]
    pub required: Option<Vec<String>>,
    #[serde(default)]
    pub label: Option<String>,
    /// The section whose card holds the dropdown (and the filled params).
    #[serde(default)]
    pub section: Option<String>,
    /// Offer *Use my own files…* (the filled params' own fields); default on.
    #[serde(default)]
    pub custom: Option<bool>,
    /// A help paragraph for the dropdown's ⓘ.
    #[serde(default)]
    pub help: Option<String>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SectionOverlay {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Collapsed under *Advanced* rather than shown as its own card.
    #[serde(default)]
    pub advanced: Option<bool>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// One param's curation. `hint` holds the widget hints shared with top-level `params`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ParamOverlay {
    #[serde(flatten)]
    pub hint: ParamHint,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub multiline: Option<bool>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    /// Named values, in order: `{ "Duplex (3)": 3, "Simplex (1)": 1 }`. A *Custom* choice is
    /// implicit unless the field is an enum.
    #[serde(default)]
    pub presets: Option<serde_norway::Mapping>,
    /// Show the param only when every listed param has the given value (or one of a list).
    #[serde(default)]
    pub show_if: Option<serde_norway::Mapping>,
    #[serde(default)]
    pub rules: Vec<RuleOverlay>,
    /// A samplesheet param's columns and format.
    #[serde(default)]
    pub samplesheet: Option<SheetOverlay>,
}

/// The author's samplesheet declaration. `columns` replaces any columns derived from the engine's
/// row schema; a column's role (sample id, read 1, read 2) defaults from its name.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SheetOverlay {
    #[serde(default)]
    pub columns: Option<Vec<ColumnDecl>>,
    #[serde(default)]
    pub format: Option<crate::samples::SheetFormat>,
    #[serde(default)]
    pub paired_only: Option<bool>,
    #[serde(default)]
    pub lanes: Option<crate::samples::Lanes>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// The `samples:` block as written: the hint the form freezes, and the keys it does not know.
/// The hint is the event contract's type, so the catch-all lives here rather than on it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SampleDecl {
    /// Flattened before [`Self::extra`], so the hint takes its own keys and only the rest are
    /// left for the catch-all.
    #[serde(flatten)]
    pub hint: biffix_core::event::SampleHint,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// One `samplesheet.columns` entry as written: the column the form freezes, and the keys it does
/// not know (kept here, not on the frozen [`crate::samples::SheetColumn`]).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ColumnDecl {
    /// Flattened before [`Self::extra`], as in [`SampleDecl`].
    #[serde(flatten)]
    pub column: crate::samples::SheetColumn,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct RuleOverlay {
    pub pattern: String,
    #[serde(default)]
    pub message: Option<String>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// A named example: a known-good set of inputs that fills the whole form.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ExampleOverlay {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
    /// A params YAML in the workflow repo, read at register time; inline `params` win over it.
    #[serde(default)]
    pub params_file: Option<String>,
    /// An `s3://` samplesheet for the version's samplesheet param.
    #[serde(default)]
    pub samplesheet: Option<String>,
    /// A reference bundle id.
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub run_name: Option<String>,
    /// Keys this struct does not know, kept only to be warned about (see
    /// [`BiffixManifest::unknown_keys`]).
    #[serde(flatten)]
    pub extra: serde_norway::Mapping,
}

/// Why `biffix.yaml` could not be read ([`BiffixManifest::parse_yaml`]): the YAML parser's message,
/// with where in the file it failed.
#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct ManifestError(String);

/// Why a workflow's engine could not be told ([`detect_engine`]).
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EngineDetectError {
    /// No `engine` in the manifest and none of the files that name one.
    #[error(
        "could not detect the engine: the repository has none of {} (Nextflow) or {} (Snakemake); \
         declare `engine: nextflow` or `engine: snakemake` in biffix.yaml",
        NEXTFLOW_MARKERS.join(", "),
        SNAKEFILE_LOCATIONS.join(", ")
    )]
    Undetected,
}

/// Where Snakemake looks for a Snakefile when none is named, in its order (its
/// `SNAKEFILE_CHOICES`): it runs the first one present. Engine detection and the CLI's default
/// entrypoint both use this list.
pub const SNAKEFILE_LOCATIONS: [&str; 4] =
    ["Snakefile", "snakefile", "workflow/Snakefile", "workflow/snakefile"];

/// The files whose presence makes a workflow Nextflow's, for engine detection.
pub const NEXTFLOW_MARKERS: [&str; 2] = ["main.nf", "nextflow.config"];

/// The workflow's engine: the manifest's `engine`, else detected from `repo_files`, the
/// repository's files as root-relative paths compared exactly (case included). Only the paths in
/// [`NEXTFLOW_MARKERS`] and [`SNAKEFILE_LOCATIONS`] are looked at, so a listing of just those
/// present will do.
pub fn detect_engine(
    manifest: &BiffixManifest,
    repo_files: &[String],
) -> Result<Engine, EngineDetectError> {
    // The manifest always overrides detection, except `auto` which falls through.
    if let Some(EngineChoice::Engine(engine)) = manifest.engine {
        return Ok(engine);
    }
    let has = |name: &str| repo_files.iter().any(|f| f == name);
    if NEXTFLOW_MARKERS.into_iter().any(has) {
        return Ok(Engine::Nextflow);
    }
    if SNAKEFILE_LOCATIONS.into_iter().any(has) {
        return Ok(Engine::Snakemake);
    }
    Err(EngineDetectError::Undetected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_standard_snakefile_location_is_detected_exactly() {
        let detect = |files: &[&str]| {
            let files: Vec<String> = files.iter().map(std::string::ToString::to_string).collect();
            detect_engine(&BiffixManifest::default(), &files)
        };
        for location in SNAKEFILE_LOCATIONS {
            assert_eq!(detect(&[location, "README.md"]), Ok(Engine::Snakemake));
        }
        // Compared exactly: another case, or the same name elsewhere, is not a Snakefile location.
        for other in ["SNAKEFILE", "Workflow/Snakefile", "rules/Snakefile"] {
            assert_eq!(detect(&[other]), Err(EngineDetectError::Undetected));
        }
        assert_eq!(detect(&["snakefile", "main.nf"]), Ok(Engine::Nextflow));
        // The error names every file it looked for, and what to declare instead.
        let message = EngineDetectError::Undetected.to_string();
        for file in NEXTFLOW_MARKERS.into_iter().chain(SNAKEFILE_LOCATIONS) {
            assert!(message.contains(file), "{file}: {message}");
        }
        assert!(message.contains("engine: snakemake"), "{message}");
    }

    #[test]
    fn a_manifest_that_does_not_parse_says_where() {
        // The parser's message alone: a caller says which file (no "bad biffix.yaml: " doubled).
        let err: ManifestError = BiffixManifest::parse_yaml("engine: [snakemake\n").unwrap_err();
        assert!(!err.to_string().contains("biffix.yaml"), "{err}");
        assert!(err.to_string().contains("line"), "{err}");
    }

    #[test]
    fn the_engine_key_is_an_engine_or_auto() {
        let engine = |yaml: &str| BiffixManifest::parse_yaml(yaml).map(|m| m.engine);
        assert_eq!(
            engine("engine: nextflow").unwrap(),
            Some(EngineChoice::Engine(Engine::Nextflow))
        );
        assert_eq!(
            engine("engine: snakemake").unwrap(),
            Some(EngineChoice::Engine(Engine::Snakemake))
        );
        assert_eq!(engine("engine: auto").unwrap(), Some(EngineChoice::Auto));
        assert_eq!(engine("{}").unwrap(), None);
        let err = engine("engine: cromwell").unwrap_err().to_string();
        assert!(err.contains("unknown engine `cromwell`"), "{err}");
    }

    #[test]
    fn form_params_hints_fold_into_params() {
        let m = BiffixManifest::parse_yaml(
            r"
params:
  reads: { format: s3-dir, suffix: .bam, secret: true }
  other: { output: true }
form:
  params:
    reads: { format: s3-file, suffix: .fastq.gz, secret: false, multiple: true }
    new: { format: samplesheet }
",
        )
        .unwrap();
        let reads = &m.params["reads"];
        // `format` and `suffix` from `form.params` replace the top-level ones ...
        assert_eq!(reads.format.as_deref(), Some("s3-file"));
        assert_eq!(reads.suffix.as_deref(), Some(".fastq.gz"));
        // ... while a boolean set in either place holds (`false` reads as unset).
        assert!(reads.secret && reads.multiple && !reads.output);
        assert!(m.params["other"].output);
        assert_eq!(m.params["new"].format.as_deref(), Some("samplesheet"));
    }

    #[test]
    fn plugins_are_nextflow_only_and_pinned() {
        let m = BiffixManifest::parse_yaml("plugins:\n  - nf-schema@2.7.2\n  - nf-prov@1.4.0\n")
            .unwrap();
        assert!(m.unknown_keys().is_empty());
        assert_eq!(
            m.nextflow_plugins(Engine::Nextflow).unwrap(),
            [
                ("nf-prov".to_string(), "1.4.0".to_string()),
                ("nf-schema".to_string(), "2.7.2".to_string())
            ]
        );
        assert!(m.nextflow_plugins(Engine::Snakemake).unwrap_err().0.contains("only to Nextflow"));
        let unpinned = BiffixManifest::parse_yaml("plugins: [nf-schema]\n").unwrap();
        assert!(unpinned
            .nextflow_plugins(Engine::Nextflow)
            .unwrap_err()
            .0
            .contains("has no version"));
        assert!(BiffixManifest::default().nextflow_plugins(Engine::Snakemake).unwrap().is_empty());
    }

    #[test]
    fn unknown_keys_are_listed() {
        let m = BiffixManifest::parse_yaml(
            r"
engine: nextflow
species: human
params:
  reads: { format: s3-file, columns: 3 }
reports:
  - { name: QC, path: qc.html, kind: html, format: html }
samples: { wildcrad: sample, tag: x, steps: { joint: study } }
catalog: { tags: [RNA-seq], verfied: true }
form:
  titel: x
  sections:
    - { id: main, hiden: true }
  params:
    reads: { show-if: { a: 1 }, label: Reads }
    sheet:
      samplesheet:
        columns:
          - { name: sample, rolee: sample_id, required: true }
          - { name: fq, file: true, suffixes: [.fq.gz] }
  examples:
    - { name: Smoke, sample: s3://b/x.csv }
  references:
    fill: { fasta: fasta }
    lable: Genome
",
        )
        .unwrap();
        assert_eq!(m.form.as_ref().unwrap().params["reads"].label.as_deref(), Some("Reads"));
        // The known keys beside an unknown one still read (`suffix` as the alias it is).
        let samples = &m.samples.as_ref().unwrap().hint;
        assert_eq!(samples.tag.as_deref(), Some("x"));
        assert_eq!(samples.steps["joint"], biffix_core::event::StepScope::Study);
        let columns = m.form.as_ref().unwrap().params["sheet"]
            .samplesheet
            .as_ref()
            .unwrap()
            .columns
            .as_ref()
            .unwrap();
        assert!(columns[0].column.required);
        assert_eq!(columns[1].column.suffixes, [".fq.gz"]);
        assert!(columns[1].extra.is_empty());
        assert_eq!(
            m.unknown_keys(),
            [
                "biffix.yaml: unknown key `species`; ignored",
                "biffix.yaml: params.reads: unknown key `columns`; ignored",
                "biffix.yaml: reports `QC`: unknown key `format`; ignored",
                "biffix.yaml: samples: unknown key `wildcrad`; ignored",
                "biffix.yaml: catalog: unknown key `verfied`; ignored",
                "biffix.yaml: form: unknown key `titel`; ignored",
                "biffix.yaml: form.sections `main`: unknown key `hiden`; ignored",
                "biffix.yaml: form.params.reads: unknown key `show-if`; ignored",
                "biffix.yaml: form.params.sheet.samplesheet.columns `sample`: unknown key `rolee`; \
                 ignored",
                "biffix.yaml: form.examples `Smoke`: unknown key `sample`; ignored",
                "biffix.yaml: form.references: unknown key `lable`; ignored",
            ]
        );
        assert!(BiffixManifest::parse_yaml("engine: snakemake\n")
            .unwrap()
            .unknown_keys()
            .is_empty());
    }

    /// The YAML library resolves scalars by the YAML 1.2 core schema, not YAML 1.1 (PyYAML, which
    /// Snakemake uses): `yes`/`on` are strings, so a manifest hint written that way is rejected
    /// instead of read as `true`. Pinned so a YAML library swap cannot change it silently.
    #[test]
    fn manifest_booleans_are_yaml_1_2() {
        let multiple = |v: &str| {
            BiffixManifest::parse_yaml(&format!("params:\n  reads: {{ multiple: {v} }}\n"))
                .map(|m| m.params["reads"].multiple)
        };
        assert!(multiple("true").unwrap());
        assert!(multiple("True").unwrap());
        assert!(!multiple("false").unwrap());
        for v in ["yes", "no", "on", "off", "y"] {
            let err = multiple(v).unwrap_err().to_string();
            assert!(err.contains("expected a boolean"), "{v}: {err}");
        }
    }

    /// How a schema or config file's scalars reach JSON (`parse_schema_text`, the same path as
    /// params files and sample sheets), pinned against a YAML library swap: YAML 1.1 booleans,
    /// leading-zero numbers and sexagesimals stay strings, `~` is null, `<<` merge keys are not
    /// applied, a duplicate key is an error, and the error names the position.
    #[test]
    fn yaml_scalar_resolution_is_pinned() {
        let (value, _) = crate::formschema::parse_schema_text(
            "a: yes\nb: on\nc: NO\nd: ~\ne:\nf: 017\ng: 0o17\nh: 0x1F\ni: 1_000\nj: 1.10\n\
             k: 2024-01-01\nl: 1:20\nm: .inf\nn: 1e3\n\
             base: &b {x: 1}\nderived:\n  <<: *b\n  y: 2\n",
        )
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "a": "yes", "b": "on", "c": "NO", "d": null, "e": null, "f": "017", "g": 15,
                "h": 31, "i": "1_000", "j": 1.1, "k": "2024-01-01", "l": "1:20", "m": null,
                "n": 1000.0,
                "base": {"x": 1}, "derived": {"<<": {"x": 1}, "y": 2},
            })
        );
        let dup = crate::formschema::parse_schema_text("a: 1\na: 2\n").unwrap_err();
        assert_eq!(dup, "duplicate entry with key \"a\"");
        let bad = crate::formschema::parse_schema_text("a: [unclosed").unwrap_err();
        assert_eq!(
            bad,
            "did not find expected ',' or ']' at line 2 column 1, \
             while parsing a flow sequence at line 1 column 4"
        );
    }
}
