// Pure logic behind the Lab view's launch wizard: the Run form, one step at a
// time. The wizard and the Expert form are two renderings of one form state, so nothing here holds
// state: it says which fields a step shows, which problems block it, and how the Review step
// summarizes the form.

import type { ReferenceBundle } from '#lib/api/index.js';
import type { FormField, FormSchema } from '#lib/workflows/schema.js';
import {
  isChanged,
  isOutdir,
  labelOf,
  sectionIdOf,
  type Card,
  type Layout,
  type Problem,
} from './form';
import {
  bundleParams,
  fillsOf,
  REFERENCE_FIELD,
  usesBundle,
  type RefChoice,
} from './referenceChoice';
import { sampleIds, sampleCount, type SheetRow } from './samplesheet/sheet';
import { isBooleanChoice, valueKey } from './validate';

export type WizardStep = 1 | 2 | 3 | 4;
/** The steps the wizard page itself renders (step 1, picking the pipeline, is `/runs/new`). */
export type FormStep = 2 | 3 | 4;

export const WIZARD_STEPS: readonly { n: WizardStep; label: string }[] = [
  { n: 1, label: 'Pick a pipeline' },
  { n: 2, label: 'Add samples' },
  { n: 3, label: 'Reference & options' },
  { n: 4, label: 'Review & launch' },
];

/** The step a `?step=` value names: 1 to 4, else the first step of the form (Add samples). */
export function parseStep(raw: string | null): WizardStep {
  const n = Number(raw);
  return n === 1 || n === 2 || n === 3 || n === 4 ? n : 2;
}

/** Whether a click is a plain one a step link handles itself (a new-tab click opens the link). */
export function plainClick(e: MouseEvent): boolean {
  return !(e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) && e.button === 0;
}

/**
 * The fields *Add samples* shows: the samplesheet params; for a pipeline without one, its input
 * files and folders (path inputs that are neither outputs, nor filled by the reference dropdown,
 * nor in a section the author marks advanced). Hidden fields never count. Every other field is set
 * on *Reference & options*, except Nextflow's output folder, which the Review step's details hold.
 */
export function sampleFields(schema: FormSchema): Set<string> {
  const sheets = schema.fields.filter((f) => f.widget === 'samplesheet' && !f.hidden);
  if (sheets.length > 0) return new Set(sheets.map((f) => f.name));
  const filled = new Set(Object.keys(schema.references?.fill ?? {}));
  // Only an author's own advanced sections: a form without sections puts every optional field
  // under an advanced *Other options*.
  const advanced = new Set((schema.sections ?? []).flatMap((s) => (s.advanced ? [s.id] : [])));
  return new Set(
    schema.fields
      .filter(
        (f) =>
          f.is_path_input &&
          !f.is_output &&
          !f.hidden &&
          !filled.has(f.name) &&
          !advanced.has(sectionIdOf(schema, f)),
      )
      .map((f) => f.name),
  );
}

/** Where each field is set: *Add samples*' fields, and those the Review step holds. */
export interface Placement {
  samples: Set<string>;
  review: Set<string>;
}

export function placeFields(schema: FormSchema, engine: string): Placement {
  const outdir = schema.fields.find((f) => isOutdir(engine, f));
  return { samples: sampleFields(schema), review: new Set(outdir ? [outdir.name] : []) };
}

/**
 * The form's layout split over the wizard's steps: *Add samples*' fields, in the layout's order
 * (from cards and Advanced alike), and the cards and Advanced groups *Reference & options* shows
 * without them. A card left empty is dropped, except the one the reference dropdown sits in
 * (`referenceSection`).
 */
export function stepCards(
  lay: Pick<Layout, 'cards' | 'advanced'>,
  at: Placement,
  referenceSection: string | null,
): { samples: FormField[]; cards: Card[]; advanced: Card[] } {
  const onSamples = (f: FormField) => at.samples.has(f.name);
  const without = (c: Card) => ({ ...c, fields: c.fields.filter((f) => !onSamples(f)) });
  return {
    samples: [...lay.cards, ...lay.advanced].flatMap((c) => c.fields).filter(onSamples),
    cards: lay.cards
      .map(without)
      .filter((c) => c.fields.length > 0 || c.section.id === referenceSection),
    advanced: lay.advanced.map(without).filter((c) => c.fields.length > 0),
  };
}

/** The step a problem's field (or the reference dropdown) is fixed on. */
export function stepOfField(name: string, at: Placement): FormStep {
  if (name === REFERENCE_FIELD) return 3;
  if (at.samples.has(name)) return 2;
  return at.review.has(name) ? 4 : 3;
}

/** The problems that block leaving `step`; the Review step is blocked by any problem. */
export function stepProblems(problems: Problem[], at: Placement, step: FormStep): Problem[] {
  return step === 4 ? problems : problems.filter((p) => stepOfField(p.field, at) === step);
}

/** The first form step with a problem blocking it, if any. */
export function firstBlockedStep(problems: Problem[], at: Placement): 2 | 3 | null {
  for (const s of [2, 3] as const) if (stepProblems(problems, at, s).length > 0) return s;
  return null;
}

/** Whether `step` can be reached: every form step before it has no problem. */
export function canReach(step: WizardStep, problems: Problem[], at: Placement): boolean {
  const blocked = firstBlockedStep(problems, at);
  return blocked === null || step <= blocked;
}

/** A value as the Review step shows it: a preset's label, Yes/No for a switch, else its text. */
export function describeValue(field: FormField, v: unknown): string {
  const preset = field.presets?.find((p) => valueKey(p.value) === valueKey(v));
  if (preset) return preset.label;
  if (isBooleanChoice(field)) return v === true || v === 'true' ? 'Yes' : 'No';
  if (typeof v === 'string') return v.trim();
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return JSON.stringify(v);
}

/** One changed option, as the Review step lists it. */
export interface ReviewOption {
  name: string;
  label: string;
  value: string;
}

/**
 * The options the Review step lists: those of `fields` sent with a value other than a fresh form's
 * (`isChanged` on the params `sent` against the params a fresh form sends, `start`). A secret is
 * never shown, only that it is set; a param left out shows the default the engine applies.
 */
export function changedOptions(
  fields: readonly FormField[],
  sent: Record<string, unknown>,
  start: Record<string, unknown>,
): ReviewOption[] {
  return fields
    .filter((f) => isChanged(f, sent, start))
    .map((f) => {
      const v = sent[f.name];
      let value: string;
      if (v === undefined)
        value =
          f.default === undefined || f.default === null || f.is_secret
            ? 'Not set'
            : `Default (${describeValue(f, f.default)})`;
      else value = f.is_secret ? 'Set' : describeValue(f, v);
      return { name: f.name, label: labelOf(f), value };
    });
}

/**
 * The samples a built sheet names, for the Review step: how many (as the server counts them), the
 * first few ids, and how many more. A sheet without a sample-id column names none.
 */
export function sampleNames(
  spec: NonNullable<FormField['samplesheet']>,
  rows: readonly SheetRow[],
  max = 5,
): { count: number; first: string[]; more: number } {
  const names = sampleIds(spec, rows);
  return {
    count: names ? names.length : sampleCount(spec, rows),
    first: (names ?? []).slice(0, max),
    more: Math.max(0, (names?.length ?? 0) - max),
  };
}

/**
 * One line of the Review step's *Samples* card: a sheet built as rows (how many samples, the first
 * few), or a param given as a value (an input file or folder, or a samplesheet file's path).
 */
export type ReviewSampleLine = { name: string; label: string } & (
  | { kind: 'rows'; count: number; first: string[]; more: number }
  | { kind: 'value'; value: string | null; sheetFile: boolean }
);

/** A param's value as the Review shows it: a list (a `multiple` path's) one item after another. */
function reviewText(v: unknown): string | null {
  if (v === undefined || v === null) return null;
  return Array.isArray(v) ? v.join(', ') : String(v);
}

/**
 * The *Samples* card's lines: one for each of *Add samples*' `fields`, from each samplesheet's mode
 * (`sheetModes`; rows unless `'path'`), its rows (`tables`) and the params the form sends.
 */
export function reviewSampleLines(
  fields: readonly FormField[],
  {
    sheetModes,
    tables,
    params,
  }: {
    sheetModes: Readonly<Record<string, 'rows' | 'path'>>;
    tables: Readonly<Record<string, SheetRow[]>>;
    params: Record<string, unknown>;
  },
): ReviewSampleLine[] {
  return fields.map((f): ReviewSampleLine => {
    if (f.widget === 'samplesheet' && f.samplesheet && sheetModes[f.name] !== 'path')
      return {
        kind: 'rows',
        name: f.name,
        label: labelOf(f),
        ...sampleNames(f.samplesheet, tables[f.name] ?? []),
      };
    const v = params[f.name];
    return {
      kind: 'value',
      name: f.name,
      label: labelOf(f),
      value: reviewText(v),
      sheetFile: f.widget === 'samplesheet',
    };
  });
}

/** The Review step's reference card: the bundle (and what it provides), or the user's own files. */
export interface ReviewReference {
  /** The card's heading: the reference dropdown's label. */
  label: string;
  name: string;
  detail: string;
  files: { name: string; label: string; value: string | null }[];
}

/**
 * The reference card for a form with a reference dropdown (`null` without one): the chosen bundle
 * (`bundle`, once loaded), or, for *Use my own files*, each param the bundle would fill (looked up
 * in `byName`, the schema's fields by name) in fill order, except those a failing `show_if` hides
 * (`inactive`).
 */
export function reviewReference(
  schema: FormSchema,
  {
    byName,
    choice,
    bundle,
    inactive,
    params,
  }: {
    byName: ReadonlyMap<string, FormField>;
    choice: RefChoice;
    bundle: ReferenceBundle | null;
    inactive: ReadonlySet<string>;
    params: Record<string, unknown>;
  },
): ReviewReference | null {
  const refs = schema.references;
  if (!refs) return null;
  const label = refs.label ?? 'Reference';
  if (usesBundle(schema, choice)) {
    const name = bundle?.display_name ?? choice.bundle;
    const fills = bundle ? fillsOf(bundle, refs).fills.map((f) => f.key) : [];
    return {
      label,
      name: name ?? 'Not chosen yet',
      detail: fills.length ? `Provides ${fills.join(', ')}` : '',
      files: [],
    };
  }
  const files = Object.keys(refs.fill)
    .map((p) => byName.get(p))
    .filter((f): f is FormField => !!f && !inactive.has(f.name))
    .map((f) => ({
      name: f.name,
      label: labelOf(f),
      value: reviewText(params[f.name]),
    }));
  return { label, name: 'Your own files', detail: '', files };
}

/**
 * The Review step's *Options*: every option the form sends (`params`) with a value other than a
 * fresh form's (`start`), whether *Reference & options* shows it or not (an author-hidden field and
 * a param the bundle lacks are listed). Left out are those the other cards name (the samples and
 * the output folder, by `at`; the reference's params) and those a failing `show_if` hides
 * (`inactive`).
 */
export function reviewOptions(
  schema: FormSchema,
  {
    at,
    choice,
    bundle,
    inactive,
    params,
    start,
  }: {
    at: Placement;
    choice: RefChoice;
    bundle: ReferenceBundle | null;
    inactive: ReadonlySet<string>;
    params: Record<string, unknown>;
    start: Record<string, unknown>;
  },
): ReviewOption[] {
  const named = usesBundle(schema, choice)
    ? bundleParams(schema, choice, bundle)
    : new Set(Object.keys(schema.references?.fill ?? {}));
  const fields = schema.fields.filter(
    (f) =>
      !at.samples.has(f.name) &&
      !at.review.has(f.name) &&
      !named.has(f.name) &&
      !inactive.has(f.name),
  );
  return changedOptions(fields, params, start);
}

/**
 * The one line the Review step's collapsed *Where results go* row reads, so the settings the design
 * keeps out of the way still say what they are: the results folder (`outdir`, the value typed;
 * blank for the analysis's own folder; a resume shares its origin's), the intermediate files'
 * retention (`retention`, the chosen option's label; `null` for an engine without the choice), and
 * that the bell says when it ends.
 */
export function reviewDetailsSummary({
  outdir,
  resume,
  retention,
}: {
  outdir: string | null;
  resume: boolean;
  retention: string | null;
}): string {
  const folder = resume
    ? 'the resumed analysis’s'
    : outdir && outdir.trim()
      ? outdir.trim()
      : 'this analysis’s own';
  const parts = [`Results folder: ${folder}`];
  if (retention)
    parts.push(`Intermediate files: ${retention[0].toLowerCase()}${retention.slice(1)}`);
  parts.push('Notify: in the app');
  return parts.join(' · ');
}
