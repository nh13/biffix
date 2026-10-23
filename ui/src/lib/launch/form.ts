// Pure logic behind the Run tab (the launch form): how a version's FormSchema lays out as cards,
// how the form's entered values become the params of `POST /runs`, the params YAML round-trip, the
// Re-run pre-fill, and the per-version draft. Kept apart from rendering so it is unit-tested.

import { parse as parseYaml, stringify as toYaml } from 'yaml';
import type { ReferenceBundle, RunSubmitRequest, WorkflowDetail } from '#lib/api/index.js';
import { localDate } from '#lib/util/date.js';
import { errorMessage } from '#lib/util/format.js';
import type { FormField, FormSchema, FormSection } from '#lib/workflows/schema.js';
import {
  inactiveFields,
  parseDecimal,
  validateParams,
  valueKey,
  type ValidationError,
} from './validate';
import {
  isEmptyRow,
  pendingSheetUri,
  problemText,
  rowName,
  validateSheet,
  type SheetRow,
} from './samplesheet/sheet';
import {
  bundleParams,
  REFERENCE_FIELD,
  referenceErrors,
  valuesForSubmit,
  usesBundle,
  valuesForValidation,
  type RefChoice,
} from './referenceChoice';
import { asRetention, type Retention } from './retention';

/** What the form holds: the values the user set, keyed by param name. Absent ⇒ the default. */
export type FormValues = Record<string, unknown>;

/**
 * What the launch form needs of a workflow version: all a registered version has, and all a form
 * preview (`biffix workflows form --open`) can supply without making up the rest.
 */
export type LaunchTarget = Pick<
  WorkflowDetail,
  'id' | 'name' | 'engine' | 'form_schema' | 'retention_days'
> &
  /** The project it runs in (its experiments are offered on the wizard's Review). */
  Partial<Pick<WorkflowDetail, 'project'>>;

/** The Nextflow param biffix defaults into the run's own results folder when it is left unset. */
export const NEXTFLOW_OUTDIR = 'outdir';

/** Turn `min_trimmed_reads` into `Min trimmed reads`. */
export function humanize(name: string): string {
  const spaced = name.replace(/[_-]+/g, ' ').trim();
  return spaced ? spaced[0].toUpperCase() + spaced.slice(1) : name;
}

export function labelOf(field: FormField): string {
  return field.label ?? humanize(field.name);
}

/** The help paragraph worth an ⓘ: absent when it only repeats the description. */
export function helpOf(field: FormField): string | null {
  const help = field.help?.trim();
  return help && help !== field.description?.trim() ? help : null;
}

/**
 * What a selected preset chip stands for, shown under the chips (`= 3 bp`): its value and the
 * field's unit. `null` when the label already says the value (`Default (10,000)`, compared without
 * separators), so the line never repeats the chip.
 */
export function presetMeaning(field: FormField, preset: { label: string; value: unknown }) {
  const value = valueKey(preset.value);
  if (value === '') return null;
  // The label states the value only as a whole token of it ("Default (10,000)" for 10000), never as
  // part of another ("Cas9" for 9, "Top 10" for 1, "Highest" for high, "1.5" for 5). A token keeps a
  // leading minus, and the `,`/`.` inside a number; a number's group commas are dropped.
  const norm = (t: string) =>
    (/^-?\d[\d,]*(\.\d+)?$/.test(t) ? t.replace(/,/g, '') : t).toLowerCase();
  const tokens = preset.label.match(/-?[\p{L}\p{N}_]+(?:[.,]\p{N}+)*/gu) ?? [];
  if (tokens.some((t) => norm(t) === norm(value))) return null;
  return `= ${value}${field.unit ? ` ${field.unit}` : ''}`;
}

/** Whether `field` is Nextflow's `outdir`, which the form offers as the run's output location. */
export function isOutdir(engine: string, field: FormField): boolean {
  return engine === 'nextflow' && field.name === NEXTFLOW_OUTDIR;
}

/**
 * The fields as the server validates them for this engine: a Nextflow `outdir` is never required
 * of the submitter, because the server defaults it into the run's prefix before it validates.
 */
export function effectiveFields(schema: FormSchema, engine: string): FormField[] {
  return schema.fields.map((f) => (isOutdir(engine, f) ? { ...f, required: false } : f));
}

const OTHER_SECTION = 'other';

/**
 * The sections of a form. A form frozen before sections existed gets two: its required fields as
 * *Inputs* and the rest as advanced *Other options*.
 */
export function sectionsOf(schema: FormSchema): FormSection[] {
  if (schema.sections && schema.sections.length > 0) {
    // A field that names no declared section is shown under a catch-all *Other options*, never
    // dropped from the form.
    const ids = new Set(schema.sections.map((s) => s.id));
    const stray = schema.fields.some((f) => !f.section || !ids.has(f.section));
    return stray && !ids.has(OTHER_SECTION)
      ? [...schema.sections, { id: OTHER_SECTION, title: 'Other options', advanced: true }]
      : schema.sections;
  }
  const out: FormSection[] = [];
  if (schema.fields.some((f) => f.required)) out.push({ id: 'inputs', title: 'Inputs' });
  if (schema.fields.some((f) => !f.required))
    out.push({ id: OTHER_SECTION, title: 'Other options', advanced: true });
  return out;
}

export function sectionIdOf(schema: FormSchema, field: FormField): string {
  if (schema.sections && schema.sections.length > 0)
    return field.section && schema.sections.some((s) => s.id === field.section)
      ? field.section
      : OTHER_SECTION;
  return field.required ? 'inputs' : OTHER_SECTION;
}

export interface Card {
  section: FormSection;
  fields: FormField[];
  /** A field in this card is sent with a value a fresh form doesn't send (the dot on a group). */
  changed: boolean;
}

export interface Layout {
  /** Shown as cards, in order. */
  cards: Card[];
  /** Collapsed together under *Advanced*, in order. */
  advanced: Card[];
  /** Nextflow's `outdir`, offered under *Run details* instead of in its section. */
  outdir: FormField | null;
}

/**
 * Whether `field` is sent with a value other than the one a fresh form sends. `sent` and `start`
 * are the params (`paramsFrom`) of the current values and of a fresh form's (its prefills); a
 * param left out runs with the engine's `default`, so clearing a prefill is a change and typing
 * the default where there is no prefill is not.
 */
export function isChanged(
  field: FormField,
  sent: Record<string, unknown>,
  start: Record<string, unknown>,
): boolean {
  const v = sent[field.name] ?? field.default ?? null;
  const base = start[field.name] ?? field.default ?? null;
  return valueKey(v) !== valueKey(base);
}

/**
 * Lay the form out for the current values: hidden fields only when `showAll` (or named in
 * `reveal`), fields whose `show_if` conditions hide them never; everything else in its section's
 * card, advanced sections collapsed together. `omit` names fields shown elsewhere (a reference
 * bundle's params); `keep` is a section whose card is shown even when it has no fields left (the
 * reference dropdown's).
 */
export function layout(
  schema: FormSchema,
  engine: string,
  values: FormValues,
  showAll: boolean,
  {
    omit = new Set<string>(),
    keep = null,
    reveal = new Set<string>(),
    inactive: hidden,
  }: {
    omit?: Set<string>;
    keep?: string | null;
    reveal?: Set<string>;
    /**
     * The fields `show_if` hides, when the caller judged it on other values than `values` (the
     * form's `launchParts`: a bundle's URIs filled in, a row-built sheet as its file), so what is
     * shown and what is checked agree.
     */
    inactive?: Set<string>;
  } = {},
): Layout {
  const sent = paramsFrom(schema, values);
  const start = paramsFrom(schema, prefillValues(schema));
  const inactive = hidden ?? inactiveFields(schema.fields, sent);
  const outdir = schema.fields.find((f) => isOutdir(engine, f)) ?? null;
  const cards: Card[] = [];
  const advanced: Card[] = [];
  for (const section of sectionsOf(schema)) {
    const fields = schema.fields.filter(
      (f) =>
        sectionIdOf(schema, f) === section.id &&
        f !== outdir &&
        !omit.has(f.name) &&
        !inactive.has(f.name) &&
        (showAll || !f.hidden || reveal.has(f.name)),
    );
    if (fields.length === 0 && section.id !== keep) continue;
    const card = { section, fields, changed: fields.some((f) => isChanged(f, sent, start)) };
    (section.advanced ? advanced : cards).push(card);
  }
  return { cards, advanced, outdir };
}

/** Whether a form value is unset: absent, null, or text that is only whitespace. */
export function isBlank(v: unknown): boolean {
  return v === undefined || v === null || (typeof v === 'string' && v.trim() === '');
}

/**
 * The params the form's values submit: blank values are left out (the engine's default applies),
 * a number typed as text becomes a number, and text is trimmed. Hidden-by-condition fields are left
 * out too (the server would drop them anyway).
 */
export function paramsFrom(schema: FormSchema, values: FormValues): Record<string, unknown> {
  const params: Record<string, unknown> = {};
  const byName = new Map(schema.fields.map((f) => [f.name, f]));
  for (const [name, raw] of Object.entries(values)) {
    if (isBlank(raw)) continue;
    const field = byName.get(name);
    let v: unknown = typeof raw === 'string' ? raw.trim() : raw;
    if (field?.widget === 'number' && typeof v === 'string') {
      // Read as the server reads it; a whole number too large to hold exactly stays as its text.
      const n = parseDecimal(v);
      if (n !== null && (Number.isSafeInteger(n) || !/^[+-]?\d+$/.test(v))) v = n;
    }
    params[name] = v;
  }
  for (const name of inactiveFields(schema.fields, params)) delete params[name];
  return params;
}

/** The errors the server would report for the form's values. */
export function validateForm(
  schema: FormSchema,
  engine: string,
  values: FormValues,
): ValidationError[] {
  return validateParams(effectiveFields(schema, engine), paramsFrom(schema, values));
}

/** Fill a run-name template: `{date}` (the local YYYY-MM-DD) and `{workflow}`. */
export function runNameFrom(
  template: string | null | undefined,
  workflow: string,
  now: Date,
): string {
  const date = localDate(now);
  const shortName = workflow.split('/').pop() ?? workflow;
  const t = template?.trim() || '{workflow} · {date}';
  return t.replaceAll('{date}', date).replaceAll('{workflow}', shortName);
}

/** What a launch sets besides its params, for the command line that repeats it. */
export interface CommandOptions {
  /** The reference bundle chosen. */
  reference?: string | null;
  /** The intermediate-files choice, when not the default (Nextflow only). */
  retention?: Retention | null;
  /** The run's name. */
  name?: string | null;
  /** The experiment it launches into. */
  experiment?: string | null;
  /** The submit-time images (a re-run's, each pinned). */
  images?: readonly string[];
}

/** `text` as one shell word: as is when plain, else single-quoted. */
function shellWord(text: string): string {
  return /^[\w.:/@+=-]+$/.test(text) ? text : `'${text.replaceAll("'", "'\\''")}'`;
}

/** The `biffix runs submit` command line that launches `workflowId` with a `params.yaml`. */
export function paramsCommand(workflowId: string, opts: CommandOptions = {}): string {
  let cmd = `biffix runs submit ${workflowId} --params params.yaml`;
  if (opts.name?.trim()) cmd += ` --name ${shellWord(opts.name.trim())}`;
  if (opts.experiment) cmd += ` --experiment ${shellWord(opts.experiment)}`;
  if (opts.reference) cmd += ` --reference ${opts.reference}`;
  if (opts.retention && opts.retention !== 'default')
    cmd += ` --retention ${opts.retention.replaceAll('_', '-')}`;
  for (const image of opts.images ?? []) cmd += ` --image ${shellWord(image)}`;
  return cmd;
}

/**
 * The params file `biffix runs submit --params` accepts for these params, headed by the command
 * that submits it. `rowSheets` names samplesheets built as rows in the form: the file cannot carry
 * them, so the header says so.
 */
export function paramsYaml(
  workflowId: string,
  params: Record<string, unknown>,
  opts: CommandOptions & { rowSheets?: string[]; secrets?: string[] } = {},
): string {
  let header = `# ${paramsCommand(workflowId, opts)}\n`;
  for (const sheet of opts.rowSheets ?? [])
    header += `# Not included: the samplesheet \`${sheet}\` built in the form; give its file as \`${sheet}\`.\n`;
  // A secret is never written to a file: the command gives it.
  const secrets = new Set(opts.secrets ?? []);
  for (const name of secrets)
    if (name in params)
      header += `# Not included: the secret \`${name}\`; add --param ${name}=… to the command.\n`;
  const kept = Object.fromEntries(Object.entries(params).filter(([k]) => !secrets.has(k)));
  return Object.keys(kept).length === 0 ? `${header}{}\n` : header + toYaml(kept, { lineWidth: 0 });
}

export interface ImportedParams {
  values: FormValues;
  /** Keys that are not params of this version (kept only when *Show all parameters* is on). */
  unknown: string[];
}

/** Read a params YAML (or JSON) mapping; throws a readable error for anything else. */
export function parseParamsYaml(text: string, schema: FormSchema): ImportedParams {
  let doc: unknown;
  try {
    doc = parseYaml(text);
  } catch (e) {
    throw new Error(`Not valid YAML: ${errorMessage(e).split('\n')[0]}`);
  }
  if (doc === null || doc === undefined) return { values: {}, unknown: [] };
  if (typeof doc !== 'object' || Array.isArray(doc))
    throw new Error('A params file is a mapping of parameter names to values.');
  const names = new Set(schema.fields.map((f) => f.name));
  const values: FormValues = { ...(doc as Record<string, unknown>) };
  return { values, unknown: Object.keys(values).filter((k) => !names.has(k)) };
}

/** What the server records in place of a secret param's value. */
export const REDACTED_SECRET_PLACEHOLDER = '[REDACTED]';

/** The facts of a finished run that pre-fill the form for a Re-run or Resume. */
export interface RunFacts {
  run_id: string;
  engine?: string | null;
  params?: unknown;
  output_prefix?: string | null;
  run_name?: string | null;
  /** Samplesheets biffix wrote for the run: param → `{uri, rows}`. */
  input_tables?: unknown;
  /** The reference bundle the run was launched with, and the URIs it filled by bundle key. */
  reference_bundle?: { id: string; uris?: Record<string, string> } | null;
  /** What the run asked for its intermediate files. */
  retention?: string | null;
  /** The images it was submitted with (`--image`): each as given and as pinned. */
  submit_images?: { declared_ref: string; source_ref: string }[] | null;
  /** The experiment it is in. */
  experiment_id?: string | null;
}

export interface Prefill {
  values: FormValues;
  /** Samplesheet rows by param, from sheets biffix wrote for the run. */
  tables: Record<string, SheetRow[]>;
  /** Secret params whose recorded value is only a placeholder: they must be entered again. */
  secrets: string[];
  runName: string;
  /** The reference choice: the run's bundle again, or its own files. */
  reference: RefChoice;
  /** The run's intermediate-files choice again. */
  retention: Retention;
  /** The prior run's submit-time images, each as given at the digest it ran ([`pinnedSpelling`]). */
  images: string[];
  /** The prior run's experiment: trying it again lands in the same one. */
  experiment: string | null;
}

/**
 * How a resume or re-run gives a prior run's submit-time image again: the spelling as given at the
 * digest it ran (`ubuntu:24.04@sha256:…`), so the same bytes run and the spelling the workflow's
 * tasks name still maps to them (the server maps a digest-carrying spelling's digest-less form
 * too). A spelling that already carries a digest is given as-is. Mirrors the CLI's.
 */
export function pinnedSpelling(image: { declared_ref: string; source_ref: string }): string {
  if (image.declared_ref.includes('@sha256:')) return image.declared_ref;
  const at = image.source_ref.lastIndexOf('@');
  const digest = at >= 0 ? image.source_ref.slice(at + 1) : '';
  return image.declared_ref && digest.startsWith('sha256:')
    ? `${image.declared_ref}@${digest}`
    : image.source_ref;
}

/** The `outdir` the server defaults a Nextflow run to (`default_nextflow_outdir`). */
export function defaultOutdir(outputPrefix: string): string {
  return `${outputPrefix.endsWith('/') ? outputPrefix : `${outputPrefix}/`}results`;
}

/**
 * The form values that re-run `run`: its recorded params, except secrets (recorded as a
 * placeholder) and, for a fresh Nextflow run, the `outdir` the server defaulted into the old run's
 * folder (a resume keeps it: it shares the folder).
 */
export function prefillFromRun(run: RunFacts, schema: FormSchema, resume: boolean): Prefill {
  const recorded =
    run.params && typeof run.params === 'object' && !Array.isArray(run.params)
      ? (run.params as Record<string, unknown>)
      : {};
  const secretNames = new Set(schema.fields.filter((f) => f.is_secret).map((f) => f.name));
  const values: FormValues = {};
  const secrets: string[] = [];
  for (const [name, value] of Object.entries(recorded)) {
    // The placeholder alone also marks a secret: the field may have been a secret when the run was
    // submitted and no longer be one in this version's form.
    if (secretNames.has(name) || value === REDACTED_SECRET_PLACEHOLDER) {
      secrets.push(name);
      continue;
    }
    values[name] = value;
  }
  if (
    !resume &&
    run.engine === 'nextflow' &&
    run.output_prefix &&
    values[NEXTFLOW_OUTDIR] === defaultOutdir(run.output_prefix)
  )
    delete values[NEXTFLOW_OUTDIR];
  // A sheet biffix wrote from rows comes back as rows, so the samples can be edited.
  const tables: Record<string, SheetRow[]> = {};
  if (run.input_tables && typeof run.input_tables === 'object') {
    for (const [param, table] of Object.entries(run.input_tables as Record<string, unknown>)) {
      const rows = (table as { rows?: unknown })?.rows;
      if (Array.isArray(rows)) {
        tables[param] = rows as SheetRow[];
        delete values[param];
      }
    }
  }
  // A run launched with a bundle picks the bundle again; the params it filled are not the user's.
  // A param whose key the bundle lacked was the user's own and stays (all go when `uris` is unknown).
  const bundleId = run.reference_bundle?.id ?? null;
  const filled = run.reference_bundle?.uris;
  if (bundleId && schema.references)
    for (const [param, key] of Object.entries(schema.references.fill))
      if (!filled || Object.hasOwn(filled, key)) delete values[param];
  // Without one it ran on its own files, which the form opens on again only if it allows them.
  const reference: RefChoice =
    bundleId || !schema.references?.custom
      ? { bundle: bundleId, custom: false }
      : { bundle: null, custom: true };
  // A named run's re-run is named after it; an unnamed one's stays unnamed, so the name derived from
  // its pipeline, start and samples shows (never a raw run id as a name).
  const base = run.run_name?.trim();
  return {
    values,
    tables,
    secrets,
    runName: !base ? '' : resume ? `${base} (resumed)` : `${base} (re-run)`,
    reference,
    retention: asRetention(run.retention),
    // A resume and a re-run alike run the prior run's submit-time images again (the tasks name
    // them); an Operator may, since they are already imported.
    images: (run.submit_images ?? []).map(pinnedSpelling),
    experiment: run.experiment_id ?? null,
  };
}

// ------------------------------------------------------------------------------------------------
// Launch: the request the form submits and the problems that block it.

/** What a launch from the form's state sends ([`launchRequest`]). */
export interface LaunchRequestInputs {
  workflowId: string;
  engine: string;
  schema: FormSchema;
  /** Its `launchParts`: the params, and the row-built sheets whose rows are sent. */
  parts: Pick<LaunchParts, 'params' | 'rowFields'>;
  runName: string;
  /** Samplesheet rows by param. */
  tables: Record<string, SheetRow[]>;
  reference: RefChoice;
  /** The run it starts from (a Re-run or Resume, a Try again), and that run's pinned images. */
  from: { runId: string; resume: boolean; images: string[] } | null;
  retention: Retention;
  experiment: string | null;
}

/**
 * The `POST /runs` request for launching the form's state: the launch form's Launch and the Lab's
 * Try again both send through this, so Try again launches exactly what the form would. A resume
 * names the run it resumes; a re-run, the run it tries again (the server marks its notifications
 * read).
 */
export function launchRequest(s: LaunchRequestInputs): RunSubmitRequest {
  const request: RunSubmitRequest = {
    workflow_version_id: s.workflowId,
    params: s.parts.params,
    run_name: s.runName.trim() || undefined,
  };
  // The row-built sheets of shown fields, leaving out empty ones.
  const tables = Object.fromEntries(
    s.parts.rowFields
      .map((f) => [f.name, s.tables[f.name] ?? []] as const)
      .filter(([, rows]) => rows.length > 0),
  );
  if (Object.keys(tables).length > 0) request.tables = tables;
  if (usesBundle(s.schema, s.reference) && s.reference.bundle)
    request.reference = s.reference.bundle;
  if (s.from?.resume) request.resume_from = s.from.runId;
  else if (s.from) request.retry_of = s.from.runId;
  if (s.from && s.from.images.length > 0) request.images = s.from.images;
  // Only Nextflow runs have intermediate files to keep or delete.
  if (s.engine === 'nextflow' && s.retention !== 'default') request.retention = s.retention;
  // Only when one is chosen, so a launch outside any experiment sends what it always has.
  if (s.experiment) request.experiment = s.experiment;
  return request;
}

/**
 * How the form shows samplesheet params pre-filled with `names`: each samplesheet param among them
 * holds a sheet file's path (an existing sheet, not rows biffix writes), so it is in path mode.
 */
export function pathSheetModes(
  schema: FormSchema,
  names: Iterable<string>,
): Record<string, 'path'> {
  const given = new Set(names);
  return Object.fromEntries(
    schema.fields
      .filter((f) => f.widget === 'samplesheet' && given.has(f.name))
      .map((f) => [f.name, 'path' as const]),
  );
}

/** The launch form's state that decides what it submits and what blocks the launch. */
export interface LaunchInputs {
  schema: FormSchema;
  engine: string;
  values: FormValues;
  /** Samplesheet rows by param. */
  tables: Record<string, SheetRow[]>;
  /** Samplesheet params switched to a sheet file's path (absent ⇒ rows). */
  sheetModes: Record<string, 'rows' | 'path'>;
  reference: RefChoice;
  /** The chosen bundle, once loaded (`null` while loading, or none chosen). */
  bundle: ReferenceBundle | null;
}

/** What the launch form derives from [`LaunchInputs`]; the intermediates are the form's own too. */
export interface LaunchParts {
  /** Samplesheet params that may be given as rows: biffix writes their file. */
  sheetRowFields: FormField[];
  /** The values as the server judges `show_if` on them (bundle filled, row sheets as files). */
  filledValues: FormValues;
  /** Fields hidden for those values. */
  inactive: Set<string>;
  /** The row-built sheets of shown fields (a hidden one is neither checked nor sent). */
  rowFields: FormField[];
  /** The values to submit, without the bundle's params and the row-built sheets. */
  submittedValues: FormValues;
  /** What is sent as `params`. */
  params: Record<string, unknown>;
  /** The validation errors the form reports. */
  errors: ValidationError[];
  /** The row-built sheets' own problems. */
  sheetProblems: Problem[];
}

/**
 * The launch form's pipeline from its state to the params it sends and the errors that block it,
 * shared by the form (`RunForm`) and by *Try again* on the Lab analysis page, so the two launch
 * the same thing.
 */
export function launchParts(s: LaunchInputs): LaunchParts {
  const { schema, engine, values, tables, sheetModes, reference, bundle } = s;
  const hasRows = (f: FormField) => (tables[f.name]?.length ?? 0) > 0;
  const sheetRowFields = schema.fields.filter(
    (f) => f.widget === 'samplesheet' && f.samplesheet && sheetModes[f.name] !== 'path',
  );
  const filledValues = valuesForValidation(schema, { ...values }, reference, bundle);
  for (const f of sheetRowFields) {
    if (hasRows(f)) filledValues[f.name] = pendingSheetUri(f.samplesheet!);
    else delete filledValues[f.name];
  }
  const params = paramsFrom(schema, filledValues);
  const inactive = inactiveFields(schema.fields, params);
  const rowFields = sheetRowFields.filter((f) => !inactive.has(f.name));
  const submittedValues = valuesForSubmit(schema, values, reference, bundle);
  for (const f of rowFields) delete submittedValues[f.name];
  for (const name of bundleParams(schema, reference, bundle)) delete params[name];
  for (const f of sheetRowFields) delete params[f.name];
  // Validation sees a row-built sheet as the file it will become, so `required` holds once it has
  // rows; the rows themselves are checked against the sheet's columns.
  const checked = valuesForValidation(schema, { ...submittedValues }, reference, bundle);
  for (const f of rowFields) if (hasRows(f)) checked[f.name] = pendingSheetUri(f.samplesheet!);
  const errors = referenceErrors(schema, reference, bundle, validateForm(schema, engine, checked));
  // A row's problem names its sample (or row), so the problem lists point at it.
  const sheetProblems = rowFields.flatMap((f) =>
    hasRows(f)
      ? validateSheet(f.samplesheet!, tables[f.name]).map((p): Problem => {
          if (p.row === null) return { field: f.name, message: p.message };
          return {
            field: f.name,
            message: problemText(f.samplesheet!, p),
            row: p.row,
            column: p.column,
            who: rowName(f.samplesheet!, tables[f.name], p.row),
          };
        })
      : [],
  );
  return {
    sheetRowFields,
    filledValues,
    inactive,
    rowFields,
    submittedValues,
    params,
    errors,
    sheetProblems,
  };
}

/**
 * Something that blocks a launch, attached to the field (or the reference dropdown) it is about. A
 * samplesheet row's problem also names the row (0-based), its column, and how the row is named to
 * the user (`who`: `Sample B`, `Row 3`), so a problem list can point at the sample.
 */
export interface Problem {
  field: string;
  message: string;
  row?: number;
  column?: string | null;
  who?: string;
}

/**
 * A problem as a problem list shows it: `Sample B · read 2 missing` for a samplesheet row's, else
 * `<the field's label>: <message>` (`label`).
 */
export function problemTitle(p: Problem, label: string): string {
  return p.who ? `${p.who} · ${p.message}` : `${label}: ${p.message}`;
}

/**
 * Every problem that blocks a launch, in the form's own field order so the footer names the first
 * one on the page first. A field's validation error wins over its upload and secret notes.
 */
export function launchProblems(
  parts: {
    errors: Problem[];
    sheets: Problem[];
    uploading: Problem[];
    reference: Problem | null;
    secretsMissing: string[];
  },
  order: Map<string, number>,
): Problem[] {
  const hasError = (field: string) => parts.errors.some((e) => e.field === field);
  return [
    ...parts.errors,
    ...parts.sheets,
    ...parts.uploading.filter((u) => !hasError(u.field)),
    ...(parts.reference ? [parts.reference] : []),
    ...parts.secretsMissing
      .filter((name) => !hasError(name))
      .map((name) => ({ field: name, message: 'secret — enter it again' })),
  ].sort((a, b) => (order.get(a.field) ?? 1e9) - (order.get(b.field) ?? 1e9));
}

/** The first problem of each field, for looking a field's problem up without re-scanning. */
export function firstProblems(problems: Problem[]): Map<string, Problem> {
  const out = new Map<string, Problem>();
  for (const p of problems) if (!out.has(p.field)) out.set(p.field, p);
  return out;
}

/**
 * The order `launchProblems` sorts by: the form's own field order, with the reference dropdown
 * where its first filled param would be.
 */
export function problemOrder(schema: FormSchema): Map<string, number> {
  const order = new Map(schema.fields.map((f, i) => [f.name, i]));
  const fill = Object.keys(schema.references?.fill ?? {});
  const first = Math.min(...fill.map((p) => order.get(p) ?? 1e9));
  order.set(REFERENCE_FIELD, first - 0.5);
  return order;
}

/**
 * Files still uploading, one problem per field: a param's path, or a file cell of rows biffix will
 * write as a sheet. `uploading` says what is left of an upload to `uri` (null: none in flight).
 */
export function uploadingProblems(
  params: Record<string, unknown>,
  sheets: { name: string; rows: readonly SheetRow[] }[],
  uploading: (uri: string) => string | null,
): { field: string; message: string }[] {
  const out: { field: string; message: string }[] = [];
  const note = (field: string, uri: unknown) => {
    if (typeof uri !== 'string' || out.some((o) => o.field === field)) return;
    const message = uploading(uri.trim());
    if (message) out.push({ field, message });
  };
  for (const [name, v] of Object.entries(params))
    for (const uri of Array.isArray(v) ? v : [v]) note(name, uri);
  for (const { name, rows } of sheets)
    for (const row of rows) for (const cell of Object.values(row)) note(name, cell);
  return out;
}

/**
 * The secrets of a re-run (`secrets`, which its params never carry) still to be entered again: only
 * those this form can take, a field of this version that the current values show and that is empty.
 */
export function secretsToReenter(
  secrets: readonly string[],
  fields: ReadonlyMap<string, FormField>,
  inactive: ReadonlySet<string>,
  params: Record<string, unknown>,
): string[] {
  return secrets.filter(
    (name) => fields.has(name) && !inactive.has(name) && params[name] === undefined,
  );
}

/**
 * The values a fresh form starts with: each field's `prefill` (the author's `form.params` default),
 * entered as if typed so it is submitted and the engine runs with it. A field without one is left
 * unset, so the engine's `default` applies, as it does for a CLI submit that omits the param.
 */
export function prefillValues(schema: FormSchema): FormValues {
  const values: FormValues = {};
  for (const f of schema.fields)
    if (f.prefill !== undefined && f.prefill !== null) values[f.name] = f.prefill;
  return values;
}

/** Load the values of `example` into a fresh form (over its prefills, as a fresh form has them). */
export function valuesFromExample(
  schema: FormSchema,
  example: NonNullable<FormSchema['examples']>[number],
): FormValues {
  const values: FormValues = { ...prefillValues(schema), ...(example.params ?? {}) };
  const sheet = schema.fields.find((f) => f.widget === 'samplesheet');
  if (sheet && example.samplesheet_uri) values[sheet.name] = example.samplesheet_uri;
  return values;
}

/** The rest of a launch an example also replaces, and what a fresh form has for each. */
export interface LaunchExtras {
  runName?: string;
  defaultRunName?: string;
  retention?: string;
  experiment?: string | null;
  /** The experiment a fresh form starts in (the link's `?experiment=`). */
  defaultExperiment?: string | null;
}

/**
 * Whether the form holds something the user entered that an example would overwrite: a param sent
 * with a value other than a fresh form's (its prefills), a samplesheet row with anything in it, a
 * chosen reference bundle, or a run name, retention or experiment other than a fresh form's.
 * Typing a value and clearing it again is nothing to lose.
 */
export function formHasInput(
  schema: FormSchema,
  {
    values,
    tables,
    reference,
    extras = {},
  }: {
    values: FormValues;
    tables: Record<string, SheetRow[]>;
    reference: RefChoice;
    extras?: LaunchExtras;
  },
): boolean {
  if (
    extras.runName !== undefined &&
    extras.runName.trim() !== (extras.defaultRunName ?? '').trim()
  )
    return true;
  if (extras.retention !== undefined && extras.retention !== 'default') return true;
  if (
    extras.experiment !== undefined &&
    (extras.experiment ?? null) !== (extras.defaultExperiment ?? null)
  )
    return true;
  const sent = paramsFrom(schema, values);
  const start = paramsFrom(schema, prefillValues(schema));
  const names = new Set([...Object.keys(sent), ...Object.keys(start)]);
  if ([...names].some((n) => valueKey(sent[n] ?? null) !== valueKey(start[n] ?? null))) return true;
  if (Object.values(tables).some((rows) => rows.some((r) => !isEmptyRow(r)))) return true;
  return !!reference.bundle;
}

// ------------------------------------------------------------------------------------------------
// Drafts: the half-filled form of one version, kept for the browser tab so a reload does not lose
// it. Storage can be unavailable (private windows, blocked site data), so every access is guarded.

export interface Draft {
  values: FormValues;
  runName: string;
  /** Samplesheet rows by param, for params built as rows. */
  tables?: Record<string, SheetRow[]>;
  /** How each samplesheet param is given (rows unless switched to a path). */
  sheetModes?: Record<string, 'rows' | 'path'>;
  reference?: RefChoice;
  retention?: Retention;
  /** The experiment chosen on the wizard's Review (absent: none). */
  experiment?: string | null;
}

const draftKey = (versionId: string) => `biffix:launch-draft:${versionId}`;

export function loadDraft(versionId: string): Draft | null {
  try {
    const raw = sessionStorage.getItem(draftKey(versionId));
    if (!raw) return null;
    const d = JSON.parse(raw) as Draft;
    const mapping = (v: unknown) => !!v && typeof v === 'object' && !Array.isArray(v);
    return mapping(d) && mapping(d.values) && typeof d.runName === 'string' ? d : null;
  } catch {
    return null;
  }
}

/** The values a draft keeps: never a secret's (it is asked for again, like on a Re-run). */
export function draftValues(schema: FormSchema, values: FormValues): FormValues {
  const secret = new Set(schema.fields.filter((f) => f.is_secret).map((f) => f.name));
  return Object.fromEntries(Object.entries(values).filter(([name]) => !secret.has(name)));
}

export function saveDraft(versionId: string, draft: Draft): void {
  try {
    sessionStorage.setItem(draftKey(versionId), JSON.stringify(draft));
  } catch {
    /* storage unavailable: the draft just does not survive a reload */
  }
}

export function clearDraft(versionId: string): void {
  try {
    sessionStorage.removeItem(draftKey(versionId));
  } catch {
    /* storage unavailable */
  }
}
