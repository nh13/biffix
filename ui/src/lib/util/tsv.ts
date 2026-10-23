// Delimited-text tables (TSV/CSV, fgbio/fgumi/Picard metrics, BED, GFF/GTF, VCF) for the output
// viewer. Pure parsing and sorting: no DOM, O(n) in the text (sorting aside), fine for a 1 MiB
// preview.

/** One table of a file: a header row, its data rows, and which columns are all-numeric. */
export interface TableSection {
  header: string[];
  rows: string[][];
  /** Per column: at least one number, and every other cell empty or a missing-value token. */
  numeric: boolean[];
  /** The comment line that opened it, after the first section (Picard's `## HISTOGRAM …`). */
  title?: string;
}

/** A parsed file: its leading comment lines and one or more table sections. */
export interface ParsedTable {
  /** Comment lines before the first data line, with the leading `#`s and one space stripped. */
  comments: string[];
  sections: TableSection[];
  delimiter: '\t' | ',';
}

/**
 * A format whose records have no header line: its columns are fixed by its spec, so their names
 * come from there. Everything else (TSV/CSV, metrics, VCF, plain text) starts with a header.
 */
export type HeaderlessFormat = 'bed' | 'gff' | 'gtf';

const COLUMNS: Record<HeaderlessFormat, readonly string[]> = {
  // UCSC BED: three required columns, nine optional ones.
  bed: [
    'chrom',
    'chromStart',
    'chromEnd',
    'name',
    'score',
    'strand',
    'thickStart',
    'thickEnd',
    'itemRgb',
    'blockCount',
    'blockSizes',
    'blockStarts',
  ],
  gff: ['seqid', 'source', 'type', 'start', 'end', 'score', 'strand', 'phase', 'attributes'],
  gtf: ['seqname', 'source', 'feature', 'start', 'end', 'score', 'strand', 'frame', 'attribute'],
};

/**
 * The headerless format a file's name (or path) says it is, by extension and ignoring `.gz`, or
 * null for a format whose first line is its header. The backend only says a file is a `table`;
 * which kind of table is read from the same name it classified.
 */
export function headerlessFormatOf(name: string): HeaderlessFormat | null {
  const lower = name.toLowerCase().replace(/\.gz$/, '');
  if (lower.lastIndexOf('/') > lower.lastIndexOf('.')) return null; // a dot in a directory only
  const dot = lower.lastIndexOf('.');
  if (dot < 0) return null; // no extension (as the backend's classification reads it)
  const ext = lower.slice(dot + 1);
  if (ext === 'bed') return 'bed';
  if (ext === 'gff' || ext === 'gff3') return 'gff';
  if (ext === 'gtf') return 'gtf';
  return null;
}

/** How many data lines the delimiter sniff looks at. */
const SNIFF_LINES = 50;
/** The share of sniffed lines that must agree on their delimiter count. */
const SNIFF_AGREEMENT = 0.9;

/** Cell values that mean "missing" rather than text, so they don't stop a column being numeric. */
const EMPTY_TOKENS = new Set(['', '?', '.', 'NA']);
/** An undefined or unbounded double as JVM (`NaN`, `-Infinity`) and Python (`nan`, `inf`) print it. */
const NON_FINITE_RE = /^[+-]?(?:nan|inf(?:inity)?)$/i;
const NUMBER_RE = /^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/;

/** A cell that holds no number (it sorts last) without making its column text. */
const isMissing = (cell: string) => EMPTY_TOKENS.has(cell) || NON_FINITE_RE.test(cell);

/** One record of the file: its text (no line ending) and its fields under one delimiter. */
interface Rec {
  text: string;
  fields: string[];
}

/** A classified record: a data line, or a break between sections (blank, or a later comment: its
 * text, the next section's title). */
type Line = { kind: 'data'; fields: string[] } | { kind: 'break'; caption?: string };

interface Classified {
  comments: string[];
  lines: Line[];
  /** The first section's header, when a leading `#` comment line is it (BED `#chrom`, …). */
  header: string[] | null;
}

/**
 * Parse `text` as a delimited table, or return null when it does not look tabular.
 *
 * `truncated` means `text` is a prefix of the file, so its last record may be partial and is
 * dropped. `headerless` names a format whose records have no header line (BED, GFF, GTF): its
 * columns take the spec's names, and blank and comment lines between records are skipped.
 * Otherwise truly blank lines (not ones of empty cells), and comment lines after the first data
 * line, split the file into sections (e.g. a Picard metrics file's METRICS and HISTOGRAM tables),
 * and each section's first line is its header. A header line may start with `#`: VCF's `#CHROM`,
 * or, in a tab-delimited file, a single-`#` comment line right above the first data line with one
 * name per column (BED `#chrom`, `samtools coverage`'s `#rname`, `bcftools query -H`).
 *
 * Tab is tried before comma. CSV fields follow RFC 4180 quoting, so a quoted field may hold commas,
 * `""` escapes and newlines.
 */
export function parseTable(
  text: string,
  truncated: boolean,
  headerless: HeaderlessFormat | null = null,
): ParsedTable | null {
  for (const delimiter of ['\t', ','] as const) {
    const recs = delimiter === '\t' ? tabRecords(text, truncated) : csvRecords(text, truncated);
    const classified = classify(recs, headerless, delimiter);
    if (!sniffs(classified, headerless !== null)) continue;
    return { comments: classified.comments, sections: build(classified, headerless), delimiter };
  }
  return null;
}

/** Tab-delimited records: one per line (quotes are ordinary characters in TSV). */
function tabRecords(text: string, truncated: boolean): Rec[] {
  const lines = text.split('\n');
  // The part after the last newline is a partial line of a truncated file ('' if none).
  if (truncated) lines.pop();
  return lines.map((raw) => {
    const line = stripCr(raw);
    return { text: line, fields: line.split('\t') };
  });
}

/** `line` without the `\r` of a CRLF ending. */
const stripCr = (line: string) => (line.endsWith('\r') ? line.slice(0, -1) : line);

/**
 * Comma-delimited records, RFC 4180 style: a field that starts with `"` runs to its closing quote,
 * across newlines, with `""` for a quote. The whole text is scanned, not line by line, so a quoted
 * newline stays inside its field. A record cut off by truncation is dropped; a quote left open at
 * the end of a complete file was not a quote after all, so the lines after it are read on their own.
 */
function csvRecords(
  text: string,
  truncated: boolean,
  openQuoteAtEnd: 'reread' | 'close' = 'reread',
): Rec[] {
  const recs: Rec[] = [];
  let fields: string[] = [];
  let field = '';
  let start = 0; // where the current record's text starts
  let inQuotes = false;
  let atFieldStart = true;
  const endRecord = (end: number) => {
    fields.push(field);
    recs.push({ text: stripCr(text.slice(start, end)), fields });
    fields = [];
    field = '';
    atFieldStart = true;
  };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inQuotes) {
      if (c !== '"') field += c;
      else if (text[i + 1] === '"') {
        field += '"';
        i++;
      } else inQuotes = false;
    } else if (c === '"' && atFieldStart) {
      inQuotes = true;
      atFieldStart = false;
    } else if (c === ',') {
      fields.push(field);
      field = '';
      atFieldStart = true;
    } else if (c === '\n') {
      endRecord(i);
      start = i + 1;
    } else if (c === '\r' && text[i + 1] === '\n') {
      // CRLF: the \n ends the record.
    } else {
      field += c;
      atFieldStart = false;
    }
  }
  if (start >= text.length) return recs; // ended on a newline (or empty): nothing partial left
  if (truncated) return recs; // the rest is a partial record
  if (inQuotes && openQuoteAtEnd === 'reread') {
    for (const raw of text.slice(start).split('\n')) {
      const line = stripCr(raw);
      recs.push(...(line === '' ? [{ text: '', fields: [''] }] : csvRecords(line, false, 'close')));
    }
    return recs;
  }
  endRecord(text.length);
  return recs;
}

/**
 * Sort records into comments, data lines and section breaks, per the rules on `parseTable`. Only a
 * tab-delimited file's `#` line can be its header: in a CSV, a comment's prose has commas.
 */
function classify(
  recs: Rec[],
  headerless: HeaderlessFormat | null,
  delimiter: '\t' | ',',
): Classified {
  const comments: string[] = [];
  const lines: Line[] = [];
  // The last leading comment, while nothing but comments has followed it.
  let candidate: Rec | null = null;
  let header: string[] | null = null;
  let seenData = false;
  for (const rec of recs) {
    const { text } = rec;
    const blank = /^ *$/.test(text); // a line of empty cells still has its delimiters
    const comment =
      !blank &&
      ((text.startsWith('#') && !text.startsWith('#CHROM')) ||
        (headerless === 'bed' && /^(?:track|browser)(?:\s|$)/.test(text)));
    if (blank || comment) {
      if (seenData) {
        // GFF3: everything after `##FASTA` is sequence, not features.
        if (headerless === 'gff' && text.startsWith('##FASTA')) break;
        if (!headerless)
          lines.push(
            comment ? { kind: 'break', caption: text.replace(/^#+ ?/, '') } : { kind: 'break' },
          );
      } else if (comment) {
        comments.push(text.replace(/^#+ ?/, ''));
        candidate = /^#(?!#)/.test(text) ? rec : null;
      } else candidate = null;
      continue;
    }
    const fields = text.startsWith('#CHROM')
      ? [rec.fields[0].slice(1), ...rec.fields.slice(1)]
      : rec.fields;
    if (!seenData && candidate && delimiter === '\t' && isHeaderFor(candidate.fields, fields)) {
      comments.pop();
      header = [candidate.fields[0].replace(/^# */, ''), ...candidate.fields.slice(1)];
    }
    seenData = true;
    lines.push({ kind: 'data', fields });
  }
  return { comments, lines, header };
}

/**
 * Whether a single-`#` comment line's fields (`#` still on the first) name the columns of the data
 * line below it: one name per column, none of them empty or a number. Deliberately narrow — a
 * `##` metadata line (Picard's `## HISTOGRAM\tjava.lang.Integer`) or one set off by a blank line
 * stays a comment, however many fields it has.
 */
function isHeaderFor(commentFields: string[], dataFields: string[]): boolean {
  if (commentFields.length < 2 || commentFields.length !== dataFields.length) return false;
  const names = [commentFields[0].replace(/^# */, ''), ...commentFields.slice(1)];
  return names.every((n) => {
    const t = n.trim();
    return t.length > 0 && !NUMBER_RE.test(t) && !isMissing(t);
  });
}

/**
 * Whether the delimiter these records were split on is consistent over the first data lines. A
 * line agrees when its delimiter count equals the most common count of its own section, so a
 * multi-section file (a wide METRICS table then a two-column HISTOGRAM) still sniffs as tabular;
 * each section's common count must be at least one. A headed file needs a header and a row; a
 * headerless one needs one record.
 */
function sniffs(c: Classified, headerless: boolean): boolean {
  const sample: number[][] = [c.header ? [c.header.length - 1] : []]; // counts, by section
  let taken = sample[0].length;
  for (const line of c.lines) {
    if (taken >= SNIFF_LINES) break;
    if (line.kind === 'break') {
      if (sample[sample.length - 1].length > 0) sample.push([]);
    } else {
      sample[sample.length - 1].push(line.fields.length - 1);
      taken++;
    }
  }
  if (taken < (headerless ? 1 : 2)) return false;
  let agreeing = 0;
  for (const counts of sample) {
    if (counts.length === 0) continue;
    const [mode, freq] = modeOf(counts);
    if (mode < 1) return false;
    agreeing += freq;
  }
  return agreeing / taken >= SNIFF_AGREEMENT;
}

/** The most common value and how often it occurs (ties go to the larger value). */
function modeOf(values: number[]): [number, number] {
  const freq = new Map<number, number>();
  for (const v of values) freq.set(v, (freq.get(v) ?? 0) + 1);
  let best = 0;
  let bestFreq = 0;
  for (const [v, f] of freq) {
    if (f > bestFreq || (f === bestFreq && v > best)) [best, bestFreq] = [v, f];
  }
  return [best, bestFreq];
}

/** The sections of classified records (a headerless file has no breaks, so it is one section). */
function build(c: Classified, headerless: HeaderlessFormat | null): TableSection[] {
  const sections: TableSection[] = [];
  let current: string[][] = [];
  // The caption of the last break before the section now being read (one after a blank stays).
  let title: string | undefined;
  const flush = () => {
    if (current.length === 0) return;
    const first = sections.length === 0;
    let section: TableSection;
    if (headerless) section = buildSection(first ? c.header : null, current, COLUMNS[headerless]);
    else if (first && c.header) section = buildSection(c.header, current, []);
    else section = buildSection(current[0], current.slice(1), []);
    if (title !== undefined) section.title = title;
    sections.push(section);
    current = [];
    title = undefined;
  };
  for (const line of c.lines) {
    if (line.kind === 'break') {
      flush();
      if (line.caption !== undefined) title = line.caption;
    } else current.push(line.fields);
  }
  flush();
  return sections;
}

/**
 * A section from its header (null ⇒ `names`) and rows. Rows are padded to the widest line; a
 * header shorter than that takes `names` (or nothing) for the rest.
 */
function buildSection(
  headerCells: string[] | null,
  lines: string[][],
  names: readonly string[],
): TableSection {
  const width = lines.reduce((w, l) => Math.max(w, l.length), headerCells?.length ?? 0);
  const pad = (cells: string[]) =>
    cells.length < width ? cells.concat(Array(width - cells.length).fill('')) : cells;
  const header = Array.from(
    { length: width },
    (_, i) => headerCells?.[i] ?? names[i] ?? (names.length > 0 ? `column ${i + 1}` : ''),
  );
  const rows = lines.map(pad);
  const numeric = header.map((_, col) => {
    let sawNumber = false;
    for (const row of rows) {
      const cell = row[col].trim();
      if (isMissing(cell)) continue;
      if (!NUMBER_RE.test(cell)) return false;
      sawNumber = true;
    }
    return sawNumber;
  });
  return { header, rows, numeric };
}

/** A numeric cell's value, or null when it is missing or not a number (those sort last). */
function numberOf(cell: string): number | null {
  const t = cell.trim();
  if (isMissing(t) || !NUMBER_RE.test(t)) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/**
 * Stable sort of rows by column `col`; numeric columns compare as numbers (missing and non-numeric
 * cells last), others with a locale collation that orders digit runs by value (`chr2` before
 * `chr10`; empty cells last). Returns a new array; `rows` is not modified.
 */
export function sortRows(
  rows: string[][],
  col: number,
  numeric: boolean,
  descending: boolean,
): string[][] {
  const sign = descending ? -1 : 1;
  // Array.prototype.sort is stable, so ties (comparator 0) keep their original order.
  if (numeric) {
    const keyed = rows.map((row) => ({ row, key: numberOf(row[col] ?? '') }));
    keyed.sort((a, b) => {
      if (a.key === null || b.key === null) return a.key === b.key ? 0 : a.key === null ? 1 : -1;
      return (a.key - b.key) * sign;
    });
    return keyed.map((k) => k.row);
  }
  const keyOf = (row: string[]) => (row[col] ?? '').trim();
  return [...rows].sort((ra, rb) => {
    const a = keyOf(ra);
    const b = keyOf(rb);
    if (a === '' || b === '') return a === b ? 0 : a === '' ? 1 : -1;
    return collator.compare(a, b) * sign;
  });
}
