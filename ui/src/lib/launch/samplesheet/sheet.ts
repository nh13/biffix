// Samplesheets in the launch form: the rows a builder edits, checked and written the way the
// server does (`biffix_workflow::samples::SheetSpec`), plus CSV/TSV import and export. The server
// re-validates on submit; these checks are for showing problems as they happen.

import type { SheetColumn, SheetSpec } from '$lib/generated/samples';

export type SheetRow = Record<string, string>;

export interface SheetProblem {
  /** 0-based row; null for the sheet as a whole. */
  row: number | null;
  column: string | null;
  message: string;
}

/** A row with every column at its default (or empty). */
export function emptyRow(spec: SheetSpec): SheetRow {
  return Object.fromEntries(spec.columns.map((c) => [c.name, c.default ?? '']));
}

/** The column holding each role, if any. */
export function roleColumn(spec: SheetSpec, role: 'sample_id' | 'read1' | 'read2') {
  return spec.columns.find((c) => c.role === role) ?? null;
}

/** A row with every cell empty (or blank): no sample, left out of the sheet (`is_empty_row`). */
export function isEmptyRow(row: SheetRow): boolean {
  return Object.values(row).every((v) => (v ?? '').trim() === '');
}

/**
 * Every problem with `rows`, as `SheetSpec::validate` finds them. An empty row is left out of the
 * sheet, so it has no problems of its own.
 */
export function validateSheet(spec: SheetSpec, rows: readonly SheetRow[]): SheetProblem[] {
  const problems: SheetProblem[] = [];
  if (rows.every(isEmptyRow)) problems.push({ row: null, column: null, message: 'no samples' });
  const known = new Set(spec.columns.map((c) => c.name));
  const seenFiles = new Map<string, { row: number; column: string }>();
  const read2 = roleColumn(spec, 'read2');
  rows.forEach((row, i) => {
    if (isEmptyRow(row)) return;
    const at = (column: string, message: string) => problems.push({ row: i, column, message });
    for (const name of Object.keys(row).sort()) {
      if (!known.has(name)) at(name, 'not a column of this samplesheet');
    }
    for (const c of spec.columns) {
      const value = (row[c.name] ?? '').trim();
      if (value === '') {
        if (c.required) at(c.name, 'required');
        continue;
      }
      if (c.enum_values && !c.enum_values.includes(value))
        at(c.name, `not one of ${c.enum_values.join(', ')}`);
      if (c.file) {
        if (!value.startsWith('s3://')) at(c.name, 'must be an s3:// path');
        else if (
          (c.suffixes?.length ?? 0) > 0 &&
          !c.suffixes!.some((s) => value.toLowerCase().endsWith(s.toLowerCase()))
        )
          at(c.name, `must end in ${c.suffixes!.join(' or ')}`);
        // Reads are each sample's own: the same read file twice (in any read column) is a mistake,
        // while a shared file (a panel, a reference) repeats by design. Each repeat names the
        // previous place with the file, as the server's map insert does.
        if (c.role === 'read1' || c.role === 'read2') {
          const previous = seenFiles.get(value);
          if (previous !== undefined)
            at(
              c.name,
              previous.row === i
                ? `the same file as ${previous.column}`
                : `the same file as row ${previous.row + 1}`,
            );
          seenFiles.set(value, { row: i, column: c.name });
        }
      }
    }
    if (spec.paired_only && read2 && (row[read2.name] ?? '').trim() === '')
      at(read2.name, 'this workflow needs paired-end reads');
  });
  return problems;
}

/**
 * The sheet as the server writes it (`SheetSpec::render`): empty rows left out, cells trimmed, and a
 * cell holding the delimiter, a quote or a line break quoted as Python's `csv` writer quotes it.
 */
export function renderSheet(spec: SheetSpec, rows: readonly SheetRow[]): string {
  const delimiter = spec.format === 'tsv' ? '\t' : ',';
  const cell = (v: string) =>
    v.includes(delimiter) || /["\n\r]/.test(v) ? `"${v.replaceAll('"', '""')}"` : v;
  const lines = [spec.columns.map((c) => cell(c.name)).join(delimiter)];
  for (const row of rows.filter((r) => !isEmptyRow(r)))
    lines.push(spec.columns.map((c) => cell((row[c.name] ?? '').trim())).join(delimiter));
  return lines.join('\n') + '\n';
}

/** A quoted field that never closes, opened on `line`. */
class UnclosedQuote extends Error {
  constructor(line: number) {
    super(`a quoted field opened on line ${line} is never closed`);
  }
}

/**
 * Split delimited text into records, honoring double-quoted fields (RFC 4180), as the server's
 * `split_records` does: a quote opens a field only at its start and a record of blank cells is
 * dropped. Returns the records before a quote that never closes, and the line it opened on
 * (`null`: none).
 */
function splitRecords(
  text: string,
  delimiter: ',' | '\t',
): { records: string[][]; unclosed: number | null } {
  const records: string[][] = [];
  let field = '';
  let record: string[] = [];
  let line = 1;
  /** The line the open quoted field opened on. */
  let quoted: number | null = null;
  let atFieldStart = true;
  const keep = () => {
    record.push(field);
    if (record.some((f) => f.trim() !== '')) records.push(record);
    record = [];
    field = '';
    atFieldStart = true;
  };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted !== null) {
      if (c === '"' && text[i + 1] === '"') {
        field += '"';
        i++;
      } else if (c === '"') quoted = null;
      else {
        if (c === '\n' || (c === '\r' && text[i + 1] !== '\n')) line++;
        field += c;
      }
    } else if (c === '"' && atFieldStart) {
      quoted = line;
      atFieldStart = false;
    } else if (c === delimiter) {
      record.push(field);
      field = '';
      atFieldStart = true;
    } else if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') i++;
      keep();
      line++;
    } else {
      field += c;
      atFieldStart = false;
    }
  }
  if (quoted !== null) return { records, unclosed: quoted };
  if (field !== '' || record.length > 0 || !atFieldStart) keep();
  return { records, unclosed: null };
}

/** A file's header and records, as read with the delimiter that fits the sheet best. */
export interface SheetTable {
  headers: string[];
  records: string[][];
}

/**
 * Read delimited text as a header and records. The delimiter is the one whose header names more of
 * the sheet's columns; when neither names any, the one that splits the header into more columns;
 * else the sheet's own format's. A byte-order mark is dropped. Throws on a quote that never closes.
 */
export function readSheetTable(text: string, spec: SheetSpec): SheetTable {
  const body = text.replace(/^﻿/, '');
  const known = new Set(spec.columns.map((c) => c.name));
  const own = spec.format === 'tsv' ? '\t' : ',';
  const other = own === '\t' ? ',' : '\t';
  // A quote left open under one delimiter (the other may not open it) counts only for the one
  // read with.
  const parsed = { [own]: splitRecords(body, own), [other]: splitRecords(body, other) } as Record<
    ',' | '\t',
    ReturnType<typeof splitRecords>
  >;
  // The header is the first non-blank record, as `SheetSpec::file_format` reads it.
  const header = (d: ',' | '\t') => parsed[d].records[0] ?? [];
  const named = (d: ',' | '\t') => header(d).filter((h) => known.has(h.trim())).length;
  let delimiter: ',' | '\t' = own;
  if (named(other) > named(own)) delimiter = other;
  else if (named(own) === 0 && named(other) === 0 && header(other).length > header(own).length)
    delimiter = other;
  const { records: all, unclosed } = parsed[delimiter];
  if (unclosed !== null) throw new UnclosedQuote(unclosed);
  const [head = [], ...records] = all;
  return { headers: head.map((h) => h.trim()), records };
}

/** Endings stripped from a sequencing file's name before it is read (`SEQ_EXTENSIONS`). */
const SEQ_EXTENSIONS = ['.fastq.gz', '.fq.gz', '.fastq', '.fq', '.bam', '.cram'];

/**
 * The lane a sequencing file's name carries (`001` for `P01_S1_L001_R1_001.fastq.gz`), as the
 * server's automatic reading finds it (`auto_read`: an `_L<lane>` right before the `R1`/`R2`
 * marker), or null.
 */
export function fileLane(uri: string): string | null {
  const name = uri.slice(uri.lastIndexOf('/') + 1);
  const ext = SEQ_EXTENSIONS.find((e) => name.toLowerCase().endsWith(e));
  const stem = ext ? name.slice(0, name.length - ext.length) : name;
  return /^.+?_L(\d{3})[._-]R[12](?:_\d{3})?$/i.exec(stem)?.[1] ?? null;
}

/**
 * Merge the rows of samples `ids` into one sample `into`: the read-1 files of one and the read-2
 * files of the other pair up (the fix for a naming slip like `P7` / `P07`, which detection
 * suggests). The server has no merge of its own; this one works on the rows. Mates pair within a
 * lane, as `SheetSpec::rows_from` pairs them: the read-1 and read-2 files of the merged rows are
 * grouped by the lane their names carry (`fileLane`), and within a lane pair up in row order into
 * one row per pair; a lane short of a read leaves that cell empty rather than borrowing another
 * lane's file. Other columns come from the first merged row. Rows are unchanged when none holds
 * either id.
 */
export function mergeSamples(
  spec: SheetSpec,
  rows: readonly SheetRow[],
  ids: readonly string[],
  into: string,
): SheetRow[] {
  const idCol = roleColumn(spec, 'sample_id');
  const r1 = roleColumn(spec, 'read1');
  const r2 = roleColumn(spec, 'read2');
  if (!idCol) return [...rows];
  const affected = rows.filter((r) => ids.includes(r[idCol.name]));
  if (affected.length === 0) return [...rows];
  const files = (col: SheetColumn | null) =>
    col ? affected.map((r) => r[col.name] ?? '').filter((v) => v.trim() !== '') : [];
  // lane → (read 1, read 2); files with no lane first, then lanes in order.
  const lanes = new Map<string, [string[], string[]]>();
  const lane = (f: string) => {
    const key = fileLane(f) ?? '';
    let reads = lanes.get(key);
    if (!reads) lanes.set(key, (reads = [[], []]));
    return reads;
  };
  for (const f of files(r1)) lane(f)[0].push(f);
  for (const f of files(r2)) lane(f)[1].push(f);
  if (lanes.size === 0) lanes.set('', [[], []]);
  const byLane = [...lanes.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const merged = byLane.flatMap(([, [read1, read2]]) =>
    Array.from({ length: Math.max(read1.length, read2.length, 1) }, (_, i) => {
      const row: SheetRow = { ...affected[0], [idCol.name]: into };
      if (r1) row[r1.name] = read1[i] ?? '';
      if (r2) row[r2.name] = read2[i] ?? '';
      return row;
    }),
  );
  const firstIndex = rows.findIndex((r) => ids.includes(r[idCol.name]));
  const rest = rows.filter((r) => !ids.includes(r[idCol.name]));
  rest.splice(firstIndex, 0, ...merged);
  return rest;
}

/** A column's heading for the table: its name, marked when required. */
export function columnLabel(c: SheetColumn): string {
  return c.required ? `${c.name} *` : c.name;
}

/** The sheet file's format, which is also its extension: `csv` unless the spec says `tsv`. */
export function sheetFormat(spec: SheetSpec): 'csv' | 'tsv' {
  return spec.format === 'tsv' ? 'tsv' : 'csv';
}

/** A placeholder param value that stands for a sheet biffix will write, for validation. */
export function pendingSheetUri(spec: SheetSpec): string {
  return `s3://biffix-pending/samplesheet.${sheetFormat(spec)}`;
}

/**
 * How many samples `rows` hold: distinct sample ids (a sample on several lanes has a row per lane
 * pair), or the number of rows when the sheet has no sample-id column. As the server counts them
 * (`SheetSpec::sample_count`): empty rows and blank ids are skipped, and ids are compared trimmed,
 * as the sheet writes them.
 */
export function sampleCount(spec: SheetSpec, rows: readonly SheetRow[]): number {
  return sampleIds(spec, rows)?.length ?? rows.filter((r) => !isEmptyRow(r)).length;
}

/**
 * The distinct sample ids `rows` name, trimmed and in first-seen order (blank ones skipped), or
 * `null` when the sheet has no sample-id column.
 */
export function sampleIds(spec: SheetSpec, rows: readonly SheetRow[]): string[] | null {
  const id = roleColumn(spec, 'sample_id');
  if (!id) return null;
  const ids = rows.map((r) => (r[id.name] ?? '').trim()).filter((v) => v !== '');
  return [...new Set(ids)];
}

/**
 * The indices in `next` of the rows that `touched` (indices into `prev`) names, followed by object
 * identity: an edit that keeps, drops or reorders rows keeps each kept row's flag with it, and a
 * row `next` does not carry over loses it.
 */
export function carryTouched(
  prev: readonly SheetRow[],
  touched: ReadonlySet<number>,
  next: readonly SheetRow[],
): Set<number> {
  const kept = new Set(prev.filter((_, i) => touched.has(i)));
  return new Set(next.flatMap((r, i) => (kept.has(r) ? [i] : [])));
}

// ------------------------------------------------------------------------------------------------
// Per-row validity: each row's status for the table's status column, and its problems in words that
// name the row's sample ("Sample B · read 2 missing"), for the table and the form's problem lists.

/** What a column holds, in words: its role's name (`read 2`), else the column's own name. */
export function columnNoun(spec: SheetSpec, column: string): string {
  const role = spec.columns.find((c) => c.name === column)?.role;
  if (role === 'read1') return 'read 1';
  if (role === 'read2') return 'read 2';
  if (role === 'sample_id') return 'sample name';
  return column;
}

/** One problem of a row in words, without the row: `read 2 missing`, `read 1: must end in .fq.gz`. */
export function problemText(spec: SheetSpec, p: SheetProblem): string {
  if (p.column === null) return p.message;
  const noun = columnNoun(spec, p.column);
  return p.message === 'required' ? `${noun} missing` : `${noun}: ${p.message}`;
}

/** The sample a row names (its trimmed sample id), or null. */
export function rowSample(spec: SheetSpec, row: SheetRow | undefined): string | null {
  const id = roleColumn(spec, 'sample_id');
  const v = id && row ? (row[id.name] ?? '').trim() : '';
  return v || null;
}

/**
 * How a row is named to the user: `Sample B` by its sample, `Sample B (row 4)` when the sample has
 * several rows (lanes), `Row 3` without one.
 */
export function rowName(spec: SheetSpec, rows: readonly SheetRow[], i: number): string {
  const sample = rowSample(spec, rows[i]);
  if (!sample) return `Row ${i + 1}`;
  const several = rows.filter((r) => rowSample(spec, r) === sample).length > 1;
  return several ? `Sample ${sample} (row ${i + 1})` : `Sample ${sample}`;
}

/** A row's state for the status column: ready, with problems, or empty (left out of the sheet). */
export interface RowStatus {
  state: 'ok' | 'problems' | 'empty';
  problems: SheetProblem[];
}

/** Every row's status, from the sheet's problems (`validateSheet`). */
export function rowStatuses(
  rows: readonly SheetRow[],
  problems: readonly SheetProblem[],
): RowStatus[] {
  const byRow = problemsByRow(problems);
  return rows.map((row, i) => {
    if (isEmptyRow(row)) return { state: 'empty', problems: [] };
    const mine = byRow.get(i) ?? [];
    return { state: mine.length > 0 ? 'problems' : 'ok', problems: mine };
  });
}

/** The 0-based rows with problems, in order. */
export function problemRows(statuses: readonly RowStatus[]): number[] {
  return statuses.flatMap((s, i) => (s.state === 'problems' ? [i] : []));
}

/**
 * The footer's summary of the rows with problems: `Problems in row 3`, `Problems in rows 3, 7 and
 * 9`, `Problems in rows 3, 7, 9 and 4 more` (1-based); empty when there are none.
 */
export function problemRowsSummary(rows: readonly number[], max = 3): string {
  if (rows.length === 0) return '';
  const shown = rows.slice(0, max).map((i) => String(i + 1));
  const more = rows.length - shown.length;
  if (rows.length === 1) return `Problems in row ${shown[0]}`;
  const list =
    more > 0
      ? `${shown.join(', ')} and ${more} more`
      : `${shown.slice(0, -1).join(', ')} and ${shown[shown.length - 1]}`;
  return `Problems in rows ${list}`;
}

// ------------------------------------------------------------------------------------------------
// The builder's table: where a row's cells are, which rows it shows, and each row's problems.

/** The id of a row's cell in the form, which the builder and the form's problem lists focus. */
export function sheetCellId(param: string, row: number, column: string): string {
  return `sheet-${param}-${row}-${column}`;
}

/** Whether `column` is one of the sheet's (a problem may name one the workflow no longer has). */
function isColumn(spec: SheetSpec, column: string | null | undefined): column is string {
  return !!column && spec.columns.some((c) => c.name === column);
}

/** The cell a row's problem is about: its column when the sheet has it, else the row's first. */
export function problemColumn(spec: SheetSpec, column: string | null | undefined): string | null {
  return isColumn(spec, column) ? column : (spec.columns[0]?.name ?? null);
}

/** The cell to show first of a row's problems: the first that names a column, else the row's first. */
export function firstProblemColumn(
  spec: SheetSpec,
  problems: readonly SheetProblem[],
): string | null {
  return problemColumn(spec, problems.find((p) => isColumn(spec, p.column))?.column);
}

/** `problems` grouped by row, once per change, so each row and cell does not scan every one. */
export function problemsByRow(problems: readonly SheetProblem[]): Map<number, SheetProblem[]> {
  const byRow = new Map<number, SheetProblem[]>();
  for (const p of problems) {
    if (p.row === null) continue;
    const list = byRow.get(p.row);
    if (list) list.push(p);
    else byRow.set(p.row, [p]);
  }
  return byRow;
}

/**
 * The rows the table shows: every one, or, under *Only rows with problems*, those with a problem
 * and those `pinned` (being fixed, or added since), so a row never vanishes while it is edited.
 */
export function shownRowIndices(
  statuses: readonly RowStatus[],
  onlyProblems: boolean,
  pinned: ReadonlySet<number>,
): number[] {
  return statuses.flatMap((s, i) =>
    !onlyProblems || s.state === 'problems' || pinned.has(i) ? [i] : [],
  );
}
