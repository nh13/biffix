import { describe, expect, it } from 'vitest';
import cases from '../../../../../schemas/sheet-cases.json';
import type { SheetSpec } from '$lib/generated/samples';
import {
  carryTouched,
  columnNoun,
  emptyRow,
  fileLane,
  firstProblemColumn,
  mergeSamples,
  problemColumn,
  problemRows,
  problemRowsSummary,
  problemsByRow,
  problemText,
  renderSheet,
  rowName,
  rowSample,
  rowStatuses,
  sampleCount,
  sheetCellId,
  shownRowIndices,
  validateSheet,
  readSheetTable,
  type SheetRow,
} from './sheet';
import { autoMapColumns, rowsFromMapping, unusedHeaders } from './sheetImport';

interface SheetCase {
  name: string;
  spec: SheetSpec;
  rows: SheetRow[];
  problems: { row?: number; column?: string; message: string }[];
  rendered: string;
  sample_count: number;
}

// The Rust samplesheet's own cases (emitted by `emit_schemas`): the port must agree on every one.

/** A file's rows as an import reads them: its table, its columns matched by name (sheetImport). */
function importRows(text: string, spec: SheetSpec) {
  const table = readSheetTable(text, spec);
  const { mapping } = autoMapColumns(spec, table.headers);
  return { rows: rowsFromMapping(spec, table.records, mapping), table, mapping };
}

describe('the UI samplesheet agrees with the Rust one', () => {
  for (const c of cases as unknown as SheetCase[]) {
    it(c.name, () => {
      expect(validateSheet(c.spec, c.rows)).toEqual(
        c.problems.map((p) => ({
          row: p.row ?? null,
          column: p.column ?? null,
          message: p.message,
        })),
      );
      expect(renderSheet(c.spec, c.rows)).toBe(c.rendered);
      expect(sampleCount(c.spec, c.rows)).toBe(c.sample_count);
    });
  }
});

const spec: SheetSpec = {
  columns: [
    { name: 'sample', role: 'sample_id', required: true },
    { name: 'fastq_1', role: 'read1', required: true, file: true, suffixes: ['.fastq.gz'] },
    { name: 'fastq_2', role: 'read2', file: true },
    {
      name: 'strandedness',
      required: true,
      enum_values: ['auto', 'forward'],
      default: 'auto',
    },
  ],
};

describe('samplesheets', () => {
  it('starts rows at their defaults', () => {
    expect(emptyRow(spec)).toEqual({ sample: '', fastq_1: '', fastq_2: '', strandedness: 'auto' });
  });

  it('validates like the server', () => {
    const rows: SheetRow[] = [
      { sample: 'a', fastq_1: 's3://b/a.fastq.gz', strandedness: 'sideways' },
      { sample: '', fastq_1: 'local/a.fastq.gz', fastq_2: 's3://b/a.txt', strandedness: 'auto' },
      { sample: 'c', fastq_1: 's3://b/a.fastq.gz', extra: 'x', strandedness: 'auto' },
    ];
    const got = validateSheet({ ...spec, paired_only: true }, rows).map(
      (p) => `${p.row}:${p.column}:${p.message}`,
    );
    expect(got).toEqual([
      '0:strandedness:not one of auto, forward',
      '0:fastq_2:this workflow needs paired-end reads',
      '1:sample:required',
      '1:fastq_1:must be an s3:// path',
      '2:extra:not a column of this samplesheet',
      '2:fastq_1:the same file as row 1',
      '2:fastq_2:this workflow needs paired-end reads',
    ]);
    expect(validateSheet(spec, [])[0].message).toBe('no samples');
    // Unknown columns in name order, as the server's sorted row map lists them.
    const unknown = {
      z: '1',
      sample: 'a',
      fastq_1: 's3://b/a.fastq.gz',
      strandedness: 'auto',
      a: '2',
    };
    expect(validateSheet(spec, [unknown]).map((p) => p.column)).toEqual(['a', 'z']);
  });

  it('quotes a cell only where its own delimiter needs it, like the server', () => {
    const row = { sample: 'a,b', fastq_1: 's3://b/a.fastq.gz', strandedness: 'auto' };
    expect(renderSheet(spec, [row])).toBe(
      'sample,fastq_1,fastq_2,strandedness\n"a,b",s3://b/a.fastq.gz,,auto\n',
    );
    const tsv: SheetSpec = { ...spec, format: 'tsv' };
    expect(renderSheet(tsv, [row])).toBe(
      'sample\tfastq_1\tfastq_2\tstrandedness\na,b\ts3://b/a.fastq.gz\t\tauto\n',
    );
    // A quote or a tab is quoted, not rewritten, so the cell reads back as it was.
    const quoted = { ...row, sample: '"S1', fastq_1: 's3://b/a\tb.fastq.gz' };
    const text = renderSheet(tsv, [quoted]);
    expect(text).toBe(
      'sample\tfastq_1\tfastq_2\tstrandedness\n"""S1"\t"s3://b/a\tb.fastq.gz"\t\tauto\n',
    );
    expect(importRows(text, tsv).rows).toEqual([{ ...quoted, fastq_2: '' }]);
  });

  it('parses quoted CSV and round-trips an export', () => {
    expect(readSheetTable('a,"b,c","d""e"\r\n\r\n1,2,3\n', spec)).toEqual({
      headers: ['a', 'b,c', 'd"e'],
      records: [['1', '2', '3']],
    });
    const rows = [
      { sample: 'P01', fastq_1: 's3://b/1.fastq.gz', fastq_2: '', strandedness: 'forward' },
    ];
    expect(importRows(renderSheet(spec, rows), spec).rows).toEqual(rows);
  });

  it('reads records as the server does (split_records)', () => {
    // A record of blank cells is no record.
    expect(readSheetTable('a,b\n  , \n1,2\n', spec)).toEqual({
      headers: ['a', 'b'],
      records: [['1', '2']],
    });
    // A quote opens a field only at its start.
    expect(readSheetTable('a,b"c\n', spec)).toEqual({ headers: ['a', 'b"c'], records: [] });
    // A quote that never closes is an error, not the rest of the file in one cell.
    expect(() => readSheetTable('sample,fastq_1\nA,"s3://b/a\nB,x\n', spec)).toThrow(
      'a quoted field opened on line 2 is never closed',
    );
  });

  it("takes the delimiter from the header, the file's first non-blank record", () => {
    const table = readSheetTable('\n\nsample\tfastq_1\nX\ts3://b/x.fastq.gz\n', spec);
    expect(table.headers).toEqual(['sample', 'fastq_1']);
    expect(table.records).toEqual([['X', 's3://b/x.fastq.gz']]);
  });

  it('imports by column name, naming unknown and missing columns', () => {
    const imported = importRows('fastq_1\tsample\tbatch\ns3://b/x.fastq.gz\tX\t7\n', spec);
    expect(imported.rows).toEqual([
      { sample: 'X', fastq_1: 's3://b/x.fastq.gz', fastq_2: '', strandedness: 'auto' },
    ]);
    expect(unusedHeaders(imported.table.headers, imported.mapping)).toEqual(['batch']);
    expect(imported.mapping.strandedness).toBeNull();
  });

  it('merges a split pair into one sample in place', () => {
    const rows = [
      { sample: 'P06', fastq_1: 's3://b/6_R1', fastq_2: 's3://b/6_R2', strandedness: 'auto' },
      { sample: 'P07', fastq_1: '', fastq_2: 's3://b/07_R2', strandedness: 'auto' },
      { sample: 'P7', fastq_1: 's3://b/7_R1', fastq_2: '', strandedness: 'auto' },
      { sample: 'P08', fastq_1: 's3://b/8_R1', fastq_2: '', strandedness: 'auto' },
    ];
    expect(mergeSamples(spec, rows, ['P7', 'P07'], 'P07')).toEqual([
      rows[0],
      { sample: 'P07', fastq_1: 's3://b/7_R1', fastq_2: 's3://b/07_R2', strandedness: 'auto' },
      rows[3],
    ]);
  });

  it('merges a split pair on several lanes into a row per lane pair, keeping every file', () => {
    // As detection writes them: a row per lane of each read-1-only and read-2-only sample.
    const rows = [
      { sample: 'P07', fastq_1: '', fastq_2: 's3://b/P07_L001_R2', strandedness: 'forward' },
      { sample: 'P07', fastq_1: '', fastq_2: 's3://b/P07_L002_R2', strandedness: 'forward' },
      { sample: 'P7', fastq_1: 's3://b/P7_L001_R1', fastq_2: '', strandedness: 'auto' },
      { sample: 'P7', fastq_1: 's3://b/P7_L002_R1', fastq_2: '', strandedness: 'auto' },
    ];
    expect(mergeSamples(spec, rows, ['P7', 'P07'], 'P07')).toEqual([
      {
        sample: 'P07',
        fastq_1: 's3://b/P7_L001_R1',
        fastq_2: 's3://b/P07_L001_R2',
        strandedness: 'forward',
      },
      {
        sample: 'P07',
        fastq_1: 's3://b/P7_L002_R1',
        fastq_2: 's3://b/P07_L002_R2',
        strandedness: 'forward',
      },
    ]);
  });

  it('pairs merged mates within a lane, never across lanes', () => {
    // P7 has read 1 on lanes 1 and 2, P07 read 2 on lane 2 only: lane 1's read 1 has no mate.
    const rows = [
      {
        sample: 'P7',
        fastq_1: 's3://b/P7_S7_L001_R1_001.fastq.gz',
        fastq_2: '',
        strandedness: 'auto',
      },
      {
        sample: 'P7',
        fastq_1: 's3://b/P7_S7_L002_R1_001.fastq.gz',
        fastq_2: '',
        strandedness: 'auto',
      },
      {
        sample: 'P07',
        fastq_1: '',
        fastq_2: 's3://b/P07_S7_L002_R2_001.fastq.gz',
        strandedness: 'auto',
      },
    ];
    expect(mergeSamples(spec, rows, ['P7', 'P07'], 'P07')).toEqual([
      {
        sample: 'P07',
        fastq_1: 's3://b/P7_S7_L001_R1_001.fastq.gz',
        fastq_2: '',
        strandedness: 'auto',
      },
      {
        sample: 'P07',
        fastq_1: 's3://b/P7_S7_L002_R1_001.fastq.gz',
        fastq_2: 's3://b/P07_S7_L002_R2_001.fastq.gz',
        strandedness: 'auto',
      },
    ]);
  });

  it('reads a lane off a file name as the server does', () => {
    expect(fileLane('s3://b/P01_S1_L001_R1_001.fastq.gz')).toBe('001');
    expect(fileLane('s3://b/P01_L002_R2.FQ.GZ')).toBe('002');
    expect(fileLane('s3://b/P01_L002.R2.fq')).toBe('002');
    expect(fileLane('s3://b/P01_S1_R1_001.fastq.gz')).toBeNull();
    expect(fileLane('s3://b/P01_L001_1.fq.gz')).toBeNull();
    expect(fileLane('s3://b/P01_L001_S1_R1.fq.gz')).toBeNull();
  });

  it('leaves rows alone when none holds either id', () => {
    const rows = [{ sample: 'A', fastq_1: 's3://b/a', fastq_2: '', strandedness: 'auto' }];
    expect(mergeSamples(spec, rows, ['P7', 'P07'], 'P07')).toEqual(rows);
    expect(mergeSamples(spec, [], ['P7', 'P07'], 'P07')).toEqual([]);
  });

  it("imports with the delimiter whose header names more columns, else the sheet's own", () => {
    // A one-column TSV: neither delimiter names more, so the sheet's TSV wins and a comma stays.
    const single: SheetSpec = { columns: [{ name: 'sample' }], format: 'tsv' };
    expect(importRows('sample\nA,B\nC\n', single).rows).toEqual([
      { sample: 'A,B' },
      { sample: 'C' },
    ]);
    // A CSV whose quoted header cell holds a tab is still read as CSV.
    const tabbed: SheetSpec = { columns: [{ name: 'a' }, { name: 'b\tc' }] };
    expect(importRows('a,"b\tc"\n1,2\n', tabbed).rows).toEqual([{ a: '1', 'b\tc': '2' }]);
    // A TSV imported into a CSV sheet.
    expect(importRows('sample\tfastq_1\nX\ts3://b/x.fastq.gz\n', spec).rows[0].fastq_1).toBe(
      's3://b/x.fastq.gz',
    );
  });
});

describe('carryTouched', () => {
  it('keeps each flag with its row through a removal, a merge and a replacement', () => {
    const [a, b, c] = [{ sample: 'a' }, { sample: 'b' }, { sample: 'c' }];
    const touched = new Set([0, 2]);
    expect([...carryTouched([a, b, c], touched, [b, c])]).toEqual([1]);
    expect([...carryTouched([a, b, c], touched, [{ sample: 'ab' }, c, a])]).toEqual([1, 2]);
    expect([...carryTouched([a, b, c], touched, [])]).toEqual([]);
    expect([...carryTouched([a, b, c], touched, [{ sample: 'a' }])]).toEqual([]);
  });
});

describe('sampleCount', () => {
  // Blank ids and ids differing only by whitespace are covered by the shared cases above.
  it('counts distinct sample ids, else rows', () => {
    const spec: SheetSpec = {
      columns: [
        { name: 'sample', role: 'sample_id' },
        { name: 'fastq_1', role: 'read1' },
      ],
    };
    expect(
      sampleCount(spec, [{ sample: 'A' }, { sample: 'A' }, { sample: 'B' }, { sample: ' ' }]),
    ).toBe(2);
    const noId: SheetSpec = { columns: [{ name: 'file' }] };
    expect(sampleCount(noId, [{ file: 'x' }, { file: 'y' }])).toBe(2);
  });
});

describe('per-row validity', () => {
  const rows: SheetRow[] = [
    { sample: 'A', fastq_1: 's3://b/A_R1.fastq.gz', fastq_2: '', strandedness: 'auto' },
    { sample: 'B', fastq_1: '', fastq_2: 's3://b/B_R2.fastq.gz', strandedness: 'auto' },
    { sample: '', fastq_1: '', fastq_2: '', strandedness: '' },
    { sample: '', fastq_1: 'local/x.fastq.gz', fastq_2: '', strandedness: 'auto' },
    { sample: 'B', fastq_1: 's3://b/B2.fastq.gz', fastq_2: '', strandedness: 'sideways' },
  ];
  const problems = validateSheet(spec, rows);

  it('gives each row a status: ready, problems, or empty', () => {
    const statuses = rowStatuses(rows, problems);
    expect(statuses.map((s) => s.state)).toEqual([
      'ok',
      'problems',
      'empty',
      'problems',
      'problems',
    ]);
    expect(statuses[1].problems.map((p) => p.column)).toEqual(['fastq_1']);
    expect(problemRows(statuses)).toEqual([1, 3, 4]);
  });

  it('says a problem in words that name what the column holds', () => {
    expect(columnNoun(spec, 'fastq_2')).toBe('read 2');
    expect(columnNoun(spec, 'sample')).toBe('sample name');
    expect(columnNoun(spec, 'strandedness')).toBe('strandedness');
    const said = problems.map((p) => problemText(spec, p));
    expect(said).toEqual([
      'read 1 missing',
      'sample name missing',
      'read 1: must be an s3:// path',
      'strandedness: not one of auto, forward',
    ]);
    expect(problemText(spec, { row: null, column: null, message: 'no samples' })).toBe(
      'no samples',
    );
  });

  it('names a row by its sample, its row when the sample has several, else its number', () => {
    expect(rowName(spec, rows, 0)).toBe('Sample A');
    expect(rowName(spec, rows, 1)).toBe('Sample B (row 2)');
    expect(rowName(spec, rows, 3)).toBe('Row 4');
    expect(rowSample(spec, rows[0])).toBe('A');
    const noId: SheetSpec = { columns: [{ name: 'file' }] };
    expect(rowName(noId, [{ file: 'x' }], 0)).toBe('Row 1');
  });

  it('summarizes the rows with problems for the footer', () => {
    expect(problemRowsSummary([])).toBe('');
    expect(problemRowsSummary([2])).toBe('Problems in row 3');
    expect(problemRowsSummary([2, 6])).toBe('Problems in rows 3 and 7');
    expect(problemRowsSummary([2, 6, 8])).toBe('Problems in rows 3, 7 and 9');
    expect(problemRowsSummary([0, 1, 2, 3, 4])).toBe('Problems in rows 1, 2, 3 and 2 more');
  });
});

describe("the builder's table", () => {
  const rows: SheetRow[] = [
    { sample: 'A', fastq_1: 's3://b/A_R1.fastq.gz', fastq_2: '', strandedness: 'auto' },
    { sample: 'B', fastq_1: '', fastq_2: '', strandedness: 'sideways' },
    { sample: '', fastq_1: '', fastq_2: '', strandedness: '' },
  ];
  const problems = validateSheet(spec, rows);
  const statuses = rowStatuses(rows, problems);

  it("names a row's cell the way the form's problem lists find it", () => {
    expect(sheetCellId('input', 3, 'fastq_1')).toBe('sheet-input-3-fastq_1');
  });

  it("points a problem at its column, else at the row's first", () => {
    expect(problemColumn(spec, 'fastq_1')).toBe('fastq_1');
    expect(problemColumn(spec, 'gone')).toBe('sample');
    expect(problemColumn(spec, null)).toBe('sample');
    expect(problemColumn({ columns: [] }, null)).toBeNull();
    const row = [
      { row: 1, column: null, message: 'x' },
      { row: 1, column: 'strandedness', message: 'y' },
    ];
    expect(firstProblemColumn(spec, row)).toBe('strandedness');
    expect(firstProblemColumn(spec, [row[0]])).toBe('sample');
  });

  it('groups problems by row, leaving out the sheet-wide ones', () => {
    const byRow = problemsByRow([{ row: null, column: null, message: 'no samples' }, ...problems]);
    expect([...byRow.keys()]).toEqual([1]);
    expect(byRow.get(1)?.map((p) => p.column)).toEqual(['fastq_1', 'strandedness']);
  });

  it('shows every row, or only those with problems and those pinned', () => {
    expect(shownRowIndices(statuses, false, new Set())).toEqual([0, 1, 2]);
    expect(shownRowIndices(statuses, true, new Set())).toEqual([1]);
    expect(shownRowIndices(statuses, true, new Set([2]))).toEqual([1, 2]);
  });
});
