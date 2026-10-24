// Importing a samplesheet CSV/TSV whose headers are not the workflow's own column names (a core's
// `Sample,R1,R2`, a sheet from another pipeline): each of the sheet's columns is matched to one of
// the file's columns, by name and by common aliases, and the user confirms or changes the match
// before anything is imported. The import parser itself is `sheet.ts`'s.

import type { SheetColumn, SheetSpec } from '#lib/generated/samples.js';
import { plural } from '#lib/util/format.js';
import { readPref, writePref } from '#lib/util/storage.js';
import { emptyRow, type SheetRow } from './sheet';

export { readSheetTable, type SheetTable } from './sheet';

/** Each sheet column → the index of the file column it takes its values from (`null`: none). */
export type ColumnMapping = Record<string, number | null>;

/** How a column was matched: its exact name, a mapping remembered from before, a similar name, or not. */
export type MatchKind = 'exact' | 'remembered' | 'similar' | 'none';

export interface AutoMapping {
  mapping: ColumnMapping;
  how: Record<string, MatchKind>;
}

/** A header as compared loosely: lower case, letters and digits only (`Read 1` ⇒ `read1`). */
export function looseName(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]/g, '');
}

/** Other names a column of a role often goes by, compared loosely. */
const ALIASES: Record<'sample_id' | 'read1' | 'read2', string[]> = {
  sample_id: ['sample', 'sampleid', 'samplename', 'id', 'name', 'samples'],
  read1: ['r1', 'read1', 'reads1', 'fastq1', 'fastqr1', 'fq1', 'forward', 'fwd', 'file1', 'fastq'],
  read2: ['r2', 'read2', 'reads2', 'fastq2', 'fastqr2', 'fq2', 'reverse', 'rev', 'file2'],
};

/**
 * Match each of the sheet's columns to a file column: the same name (always: a header that names a
 * sheet column exactly maps to it, whatever was remembered), else one the user mapped it to before
 * (`remembered`, file header → sheet column; a header remembered as '' was left out, and is not
 * guessed into a column again), else the same name compared loosely, else a common alias of its
 * role (`R1` for read 1). Each file column is used once, by the first sheet column that claims it,
 * in that order of preference.
 */
export function autoMapColumns(
  spec: SheetSpec,
  headers: readonly string[],
  remembered: Readonly<Record<string, string>> = {},
): AutoMapping {
  const mapping: ColumnMapping = Object.fromEntries(spec.columns.map((c) => [c.name, null]));
  const how: Record<string, MatchKind> = Object.fromEntries(
    spec.columns.map((c) => [c.name, 'none' as MatchKind]),
  );
  const used = new Set<number>();
  const claim = (
    kind: MatchKind,
    test: (c: SheetColumn, header: string) => boolean,
    { keepLeftOut = true } = {},
  ) => {
    for (const c of spec.columns) {
      if (mapping[c.name] !== null) continue;
      const i = headers.findIndex(
        (h, j) => !used.has(j) && h !== '' && (!keepLeftOut || remembered[h] !== '') && test(c, h),
      );
      if (i < 0) continue;
      mapping[c.name] = i;
      how[c.name] = kind;
      used.add(i);
    }
  };
  claim('exact', (c, h) => h === c.name, { keepLeftOut: false });
  claim('remembered', (c, h) => remembered[h] === c.name);
  claim('similar', (c, h) => looseName(h) === looseName(c.name));
  claim('similar', (c, h) => !!c.role && ALIASES[c.role].includes(looseName(h)));
  return { mapping, how };
}

/** The sheet column a file header names by itself (its name, loosely, or an alias of its role:
 * `R2` names read 2), with no memory; `null` when it names none. */
export function columnNamedBy(spec: SheetSpec, header: string): string | null {
  const loose = looseName(header);
  return (
    spec.columns.find((c) => c.name === header)?.name ??
    spec.columns.find((c) => looseName(c.name) === loose)?.name ??
    spec.columns.find((c) => !!c.role && ALIASES[c.role].includes(loose))?.name ??
    null
  );
}

/**
 * Whether an import should stop to show the mapping before it imports: a column was matched only
 * by a similar name (the user should see that guess); a required column without a default has no
 * match while the file has a column left over that could be it; or what was remembered would
 * quietly overrule the file's own headers: a header remembered into a column other than the one it
 * names (`R1` into read 2, a swap), or a header remembered as left out that names a sheet column
 * (`R2` dropped, which would import paired reads as single-end).
 */
export function needsMapping(
  spec: SheetSpec,
  headers: readonly string[],
  auto: AutoMapping,
  remembered: Readonly<Record<string, string>> = {},
) {
  if (Object.values(auto.how).includes('similar')) return true;
  const used = new Set(Object.values(auto.mapping).filter((i): i is number => i !== null));
  for (const [column, kind] of Object.entries(auto.how)) {
    const i = auto.mapping[column];
    if (kind !== 'remembered' || i === null) continue;
    const named = columnNamedBy(spec, headers[i]);
    if (named !== null && named !== column) return true;
  }
  if (headers.some((h, i) => !used.has(i) && remembered[h] === '' && columnNamedBy(spec, h)))
    return true;
  const leftOver = headers.some((h, i) => h !== '' && !used.has(i));
  return (
    leftOver && spec.columns.some((c) => c.required && !c.default && auto.mapping[c.name] === null)
  );
}

/** Of the file's columns no sheet column takes, those that name a sheet column by themselves
 * (`R2`: left out by the user's choice, never because the workflow does not use it), and the rest
 * (columns the workflow does not use). */
export function leftOutHeaders(
  spec: SheetSpec,
  headers: readonly string[],
  mapping: ColumnMapping,
): { chosen: string[]; unused: string[] } {
  const all = unusedHeaders(headers, mapping);
  return {
    chosen: all.filter((h) => columnNamedBy(spec, h) !== null),
    unused: all.filter((h) => columnNamedBy(spec, h) === null),
  };
}

/** The rows `records` make under `mapping`: unmapped columns keep their defaults, cells trimmed. */
export function rowsFromMapping(
  spec: SheetSpec,
  records: readonly string[][],
  mapping: ColumnMapping,
): SheetRow[] {
  return records.map((r) => {
    const row = emptyRow(spec);
    for (const c of spec.columns) {
      const i = mapping[c.name];
      if (i !== null && i !== undefined) row[c.name] = (r[i] ?? '').trim();
    }
    return row;
  });
}

/** The file columns no sheet column takes (left out of the import). */
export function unusedHeaders(headers: readonly string[], mapping: ColumnMapping): string[] {
  const used = new Set(Object.values(mapping));
  return headers.filter((h, i) => h !== '' && !used.has(i));
}

/**
 * `mapping` with sheet `column` filled by file column `value` (its index as the select gives it, or
 * '' for none). A file column feeds one sheet column: taking it elsewhere unmaps it there.
 */
export function remapColumn(mapping: ColumnMapping, column: string, value: string): ColumnMapping {
  const i = value === '' ? null : Number(value);
  const next = { ...mapping };
  if (i !== null) for (const [c, j] of Object.entries(next)) if (j === i) next[c] = null;
  next[column] = i;
  return next;
}

/** What an import of `count` rows from `file` under `mapping` says it did and left out. */
export function importNotes(
  spec: SheetSpec,
  file: string,
  headers: readonly string[],
  mapping: ColumnMapping,
  count: number,
): string {
  const notes = [`Imported ${plural(count, 'row')} from ${file}.`];
  // A column that names one of the sheet's (R2) was left out by choice, not unused.
  const { chosen, unused } = leftOutHeaders(spec, headers, mapping);
  if (unused.length)
    notes.push(`Skipped columns this workflow does not use: ${unused.join(', ')}.`);
  if (chosen.length) notes.push(`Left out, as you chose: ${chosen.join(', ')}.`);
  const missing = spec.columns.filter((c) => c.required && mapping[c.name] === null);
  if (missing.length)
    notes.push(`The file has no ${missing.map((c) => c.name).join(', ')} column.`);
  return notes.join(' ');
}

/**
 * The mapping to remember for next time: file header → the sheet column it fills, or '' for a file
 * column the user left out (so it is not guessed into a column again).
 */
export function mappingToRemember(
  headers: readonly string[],
  mapping: ColumnMapping,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const h of unusedHeaders(headers, mapping)) out[h] = '';
  for (const [column, i] of Object.entries(mapping))
    if (i !== null && headers[i]) out[headers[i]] = column;
  return out;
}

const memoryKey = (key: string) => `biffix:sheet-mapping:${key}`;

/** The mapping remembered for `key` (a pipeline's samplesheet param), or none. */
export function loadRememberedMapping(key: string): Record<string, string> {
  try {
    const raw = readPref(memoryKey(key));
    const parsed = raw ? JSON.parse(raw) : null;
    if (!parsed || typeof parsed !== 'object') return {};
    return Object.fromEntries(
      Object.entries(parsed).filter((e): e is [string, string] => typeof e[1] === 'string'),
    );
  } catch {
    return {};
  }
}

export function saveRememberedMapping(key: string, remembered: Record<string, string>): void {
  // Storage unavailable: the mapping is asked for again next time.
  writePref(memoryKey(key), JSON.stringify(remembered));
}
