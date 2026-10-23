// *Inputs used* on the Lab analysis page: the samplesheets biffix wrote for the run,
// the files it was given, the options it ran with that differ from the pipeline's standard
// settings, and every setting it ran with (behind *Show all settings*). Pure, so it is unit-tested.

import { changedOptions, describeValue, sampleNames } from '#lib/launch/wizard.js';
import {
  isChanged,
  isOutdir,
  labelOf,
  paramsFrom,
  prefillValues,
  sectionIdOf,
  sectionsOf,
} from '#lib/launch/form.js';
import type { SheetRow } from '#lib/launch/samplesheet/sheet.js';
import { asRecord } from '#lib/runs/runFacts.js';
import type { FormField, FormSchema } from '#lib/workflows/schema.js';

/** A samplesheet biffix wrote from rows: its param, spec and rows, and the samples it names. */
export interface UsedSheet {
  param: string;
  spec: NonNullable<FormField['samplesheet']>;
  rows: SheetRow[];
  count: number;
  first: string[];
  more: number;
}

/** A row-built samplesheet as a downloaded file: `<param>.tsv` or `.csv`, with its media type. */
export function sheetFile(
  param: string,
  format: string | undefined,
): { name: string; type: string } {
  return format === 'tsv'
    ? { name: `${param}.tsv`, type: 'text/tab-separated-values' }
    : { name: `${param}.csv`, type: 'text/csv' };
}

/** The run's row-built samplesheets the version's form describes (its `input_tables`). */
export function usedSheets(inputTables: unknown, schema: FormSchema): UsedSheet[] {
  const sheets: UsedSheet[] = [];
  for (const [param, table] of Object.entries(asRecord(inputTables))) {
    const rows = asRecord(table).rows;
    const spec = schema.fields.find((f) => f.name === param)?.samplesheet;
    if (!spec || !Array.isArray(rows) || rows.length === 0) continue;
    const named = sampleNames(spec, rows as SheetRow[], 3);
    sheets.push({ param, spec, rows: rows as SheetRow[], ...named });
  }
  return sheets;
}

/**
 * The options the run set differently from a fresh launch form: not its samples, outputs or the
 * params its reference bundle filled (shown on their own lines), and a secret only as *Set*.
 */
export function usedOptions(
  params: unknown,
  schema: FormSchema,
  engine: string,
  usedBundle: boolean,
): { name: string; label: string; value: string }[] {
  const filled = new Set(usedBundle ? Object.keys(schema.references?.fill ?? {}) : []);
  const fields = schema.fields.filter(
    (f) =>
      f.widget !== 'samplesheet' && !f.is_output && !isOutdir(engine, f) && !filled.has(f.name),
  );
  return changedOptions(fields, asRecord(params), paramsFrom(schema, prefillValues(schema)));
}

/** An `s3://` URI, or (for a list) each of its `s3://` URIs once, in order; else nothing. */
export function s3Uris(v: unknown): string[] {
  const one = (x: unknown) => typeof x === 'string' && /^s3:\/\/[^/]+\//.test(x.trim());
  if (one(v)) return [(v as string).trim()];
  if (Array.isArray(v) && v.length > 0 && v.every(one))
    return [...new Set(v.map((x) => (x as string).trim()))];
  return [];
}

/** One input file (or folder) setting the run was given, with the S3 locations it names. */
export interface UsedFile {
  name: string;
  label: string;
  uris: string[];
}

/**
 * The files and folders the run was given in its settings, each by its field's label: the path
 * inputs that are not outputs, nor a samplesheet biffix wrote from rows (`sheetParams`, shown as
 * samples), nor a reference file its bundle filled. In the form's order.
 */
export function usedFiles(
  params: unknown,
  schema: FormSchema,
  engine: string,
  opts: { usedBundle: boolean; sheetParams: ReadonlySet<string> },
): UsedFile[] {
  const sent = asRecord(params);
  const filled = new Set(opts.usedBundle ? Object.keys(schema.references?.fill ?? {}) : []);
  return schema.fields.flatMap((f) => {
    if (!f.is_path_input || f.is_output || isOutdir(engine, f)) return [];
    if (filled.has(f.name) || opts.sheetParams.has(f.name)) return [];
    const uris = s3Uris(sent[f.name]);
    return uris.length ? [{ name: f.name, label: labelOf(f), uris }] : [];
  });
}

/** One setting as *Show all settings* lists it. */
export interface SettingRow {
  name: string;
  label: string;
  /** As the Review step words it: the value, `Default (…)`, `Not set`, or `Set` for a secret. */
  value: string;
  /** The S3 locations the value names (linked into the Data page). */
  uris: string[];
  /** Whether it differs from what a fresh launch form sends. */
  changed: boolean;
  /** Filled from the run's reference bundle. */
  fromReference: boolean;
  description: string | null;
}

/** A card of *Show all settings*: a section of the form, as the launch form lays it out. */
export interface SettingSection {
  id: string;
  title: string;
  rows: SettingRow[];
}

/** A value of a param the version's form does not describe, as text. */
function rawValue(v: unknown): string {
  if (typeof v === 'string') return v;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return JSON.stringify(v);
}

/**
 * Every setting the run ran with, by the form's sections and in its order: each field's value (its
 * default when it was not sent), marked when it differs from a fresh form's, a secret only as set.
 * A field the form hides is listed only when the run sent it, and the samplesheets biffix wrote
 * from rows (`sheetParams`) are left out (shown as samples). Params the form does not describe
 * (sent by the CLI or an older form) close the list under *Other settings*.
 */
export function allSettings(
  params: unknown,
  schema: FormSchema,
  opts: { usedBundle: boolean; sheetParams: ReadonlySet<string> },
): SettingSection[] {
  const sent = asRecord(params);
  const start = paramsFrom(schema, prefillValues(schema));
  const filled = new Set(opts.usedBundle ? Object.keys(schema.references?.fill ?? {}) : []);
  const rowOf = (f: FormField): SettingRow => {
    const v = sent[f.name];
    const unset = v === undefined || v === null;
    const value = unset
      ? f.default === undefined || f.default === null || f.is_secret
        ? 'Not set'
        : `Default (${describeValue(f, f.default)})`
      : f.is_secret
        ? 'Set'
        : describeValue(f, v);
    return {
      name: f.name,
      label: labelOf(f),
      value,
      uris: f.is_secret ? [] : s3Uris(unset ? f.default : v),
      changed: isChanged(f, sent, start),
      fromReference: filled.has(f.name),
      description: f.description?.trim() || null,
    };
  };
  const shown = schema.fields.filter(
    (f) => !opts.sheetParams.has(f.name) && (!f.hidden || sent[f.name] !== undefined),
  );
  const sections: SettingSection[] = sectionsOf(schema)
    .map((s) => ({
      id: s.id,
      title: s.title,
      rows: shown.filter((f) => sectionIdOf(schema, f) === s.id).map(rowOf),
    }))
    .filter((s) => s.rows.length > 0);
  const described = new Set(schema.fields.map((f) => f.name));
  const others = Object.entries(sent).filter(([k]) => !described.has(k));
  if (others.length > 0)
    sections.push({
      id: '__undescribed',
      title: 'Other settings',
      rows: others.map(([name, v]) => ({
        name,
        label: name,
        value: rawValue(v),
        uris: s3Uris(v),
        changed: true,
        fromReference: false,
        description: null,
      })),
    });
  return sections;
}

/** How many settings `sections` list. */
export const settingCount = (sections: readonly SettingSection[]): number =>
  sections.reduce((n, s) => n + s.rows.length, 0);

/**
 * Whether a resumed analysis wrote its results into the folder of the one it continues: said only
 * once that one is loaded (unknown, or unreadable, says nothing), comparing the two folders without
 * a trailing `/` (as `same_output_prefix` in crates/biffix-backend/src/runs/output_prefix.rs does).
 * The one place both the resumed note
 * and *Inputs used* decide it.
 */
export function sharesOriginFolder(
  run: { resumed_from?: string | null; output_prefix?: string | null },
  origin: { output_prefix?: string | null } | null | undefined,
): boolean {
  const bare = (p: string | null | undefined) => p?.trim().replace(/\/+$/, '') || null;
  const mine = bare(run.output_prefix);
  return !!run.resumed_from && !!mine && !!origin && bare(origin.output_prefix) === mine;
}
