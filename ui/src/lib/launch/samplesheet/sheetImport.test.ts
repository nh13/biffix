import { describe, expect, it } from 'vitest';
import type { SheetSpec } from '#lib/generated/samples.js';
import {
  autoMapColumns,
  columnNamedBy,
  importNotes,
  leftOutHeaders,
  looseName,
  mappingToRemember,
  needsMapping,
  readSheetTable,
  remapColumn,
  rowsFromMapping,
  unusedHeaders,
} from './sheetImport';

const spec: SheetSpec = {
  columns: [
    { name: 'sample', role: 'sample_id', required: true },
    { name: 'fastq_1', role: 'read1', required: true, file: true },
    { name: 'fastq_2', role: 'read2', file: true },
    { name: 'strandedness', required: true, default: 'auto', enum_values: ['auto', 'forward'] },
  ],
};

describe('column mapping', () => {
  it('compares names loosely', () => {
    expect(looseName('Read 1')).toBe('read1');
    expect(looseName('FASTQ_R2')).toBe('fastqr2');
  });

  it('says which sheet column a header names by itself', () => {
    expect(columnNamedBy(spec, 'fastq_2')).toBe('fastq_2');
    expect(columnNamedBy(spec, 'Sample ID')).toBe('sample');
    expect(columnNamedBy(spec, 'R2')).toBe('fastq_2');
    expect(columnNamedBy(spec, 'Lane')).toBeNull();
  });

  it('matches exact names first, then loose names, then aliases of a role', () => {
    const { mapping, how } = autoMapColumns(spec, ['Sample', 'R1', 'R2', 'batch']);
    expect(mapping).toEqual({ sample: 0, fastq_1: 1, fastq_2: 2, strandedness: null });
    expect(how).toEqual({
      sample: 'similar',
      fastq_1: 'similar',
      fastq_2: 'similar',
      strandedness: 'none',
    });
    const exact = autoMapColumns(spec, ['fastq_1', 'sample', 'FASTQ-2']);
    expect(exact.mapping).toEqual({ sample: 1, fastq_1: 0, fastq_2: 2, strandedness: null });
    expect(exact.how.sample).toBe('exact');
    expect(exact.how.fastq_2).toBe('similar');
  });

  it('uses each file column once, and a remembered mapping wins', () => {
    // `Sample` would match `sample` loosely, but the user mapped `Donor` to it before.
    const { mapping, how } = autoMapColumns(spec, ['Sample', 'Donor', 'fq1'], {
      Donor: 'sample',
    });
    expect(mapping.sample).toBe(1);
    expect(how.sample).toBe('remembered');
    expect(mapping.fastq_1).toBe(2);
    // `Sample` is left over: no other column takes it.
    expect(unusedHeaders(['Sample', 'Donor', 'fq1'], mapping)).toEqual(['Sample']);
  });

  it('stops to show the match only when it guessed, or a required column is unmatched', () => {
    const exact = ['sample', 'fastq_1', 'fastq_2', 'batch'];
    // Every column by its own name; strandedness has a default; `batch` is just skipped.
    expect(needsMapping(spec, exact, autoMapColumns(spec, exact))).toBe(false);
    const guessed = ['Sample', 'R1'];
    expect(needsMapping(spec, guessed, autoMapColumns(spec, guessed))).toBe(true);
    // A required column with no match while a column is left over that could be it.
    const odd = ['sample', 'reads_forward'];
    expect(needsMapping(spec, odd, autoMapColumns(spec, odd))).toBe(true);
    // ...but not when nothing is left over to map.
    const short = ['sample'];
    expect(needsMapping(spec, short, autoMapColumns(spec, short))).toBe(false);
    // A remembered match is the user's own, so it imports without asking again.
    const remembered = autoMapColumns(spec, ['Donor', 'fastq_1'], { Donor: 'sample' });
    expect(needsMapping(spec, ['Donor', 'fastq_1'], remembered)).toBe(false);
  });

  it('builds rows from a mapping, keeping defaults for unmapped columns', () => {
    const records = [
      [' A ', 's3://b/A_R1.fastq.gz', 's3://b/A_R2.fastq.gz', '7'],
      ['B', 's3://b/B_R1.fastq.gz'],
    ];
    const mapping = { sample: 0, fastq_1: 1, fastq_2: 2, strandedness: null };
    expect(rowsFromMapping(spec, records, mapping)).toEqual([
      {
        sample: 'A',
        fastq_1: 's3://b/A_R1.fastq.gz',
        fastq_2: 's3://b/A_R2.fastq.gz',
        strandedness: 'auto',
      },
      { sample: 'B', fastq_1: 's3://b/B_R1.fastq.gz', fastq_2: '', strandedness: 'auto' },
    ]);
    expect(mappingToRemember(['Sample', 'R1', 'R2', 'Lane'], mapping)).toEqual({
      Sample: 'sample',
      R1: 'fastq_1',
      R2: 'fastq_2',
      Lane: '',
    });
  });

  it('does not guess a column the user left out before, but asks when it names a sheet column', () => {
    const headers = ['Sample', 'R1', 'R2'];
    const remembered = { Sample: 'sample', R1: 'fastq_1', R2: '' };
    const again = autoMapColumns(spec, headers, remembered);
    expect(again.mapping).toEqual({ sample: 0, fastq_1: 1, fastq_2: null, strandedness: null });
    // R2 names read 2: leaving it out would import paired reads as single-end, so the match shows.
    expect(needsMapping(spec, headers, again, remembered)).toBe(true);
    expect(leftOutHeaders(spec, [...headers, 'Lane'], again.mapping)).toEqual({
      chosen: ['R2'],
      unused: ['Lane'],
    });
    // A column that names nothing of the sheet stays out without asking.
    const lane = ['sample', 'fastq_1', 'Lane'];
    const out = autoMapColumns(spec, lane, { Lane: '' });
    expect(needsMapping(spec, lane, out, { Lane: '' })).toBe(false);
  });

  it('never lets a remembered match overrule a header that names a column exactly, or swap roles', () => {
    // Remembered from one mislabelled file: R1 and R2 swapped.
    const swapped = { R1: 'fastq_2', R2: 'fastq_1', fastq_1: 'fastq_2', fastq_2: 'fastq_1' };
    // The standard headers map by their own names, whatever was remembered.
    const exact = ['sample', 'fastq_1', 'fastq_2'];
    const e = autoMapColumns(spec, exact, swapped);
    expect(e.mapping).toEqual({ sample: 0, fastq_1: 1, fastq_2: 2, strandedness: null });
    expect(e.how.fastq_1).toBe('exact');
    expect(needsMapping(spec, exact, e, swapped)).toBe(false);
    // An exact header remembered as left out still maps.
    const kept = autoMapColumns(spec, exact, { fastq_2: '' });
    expect(kept.mapping.fastq_2).toBe(2);
    // Aliases remembered into the other read: the swap is shown, not imported quietly.
    const aliased = ['sample', 'R1', 'R2'];
    const a = autoMapColumns(spec, aliased, swapped);
    expect(a.mapping).toEqual({ sample: 0, fastq_1: 2, fastq_2: 1, strandedness: null });
    expect(needsMapping(spec, aliased, a, swapped)).toBe(true);
  });

  it("reads a file whose headers name none of the sheet's columns with the delimiter that splits it", () => {
    const tsv: SheetSpec = { ...spec, format: 'tsv' };
    expect(readSheetTable('Sample,R1\nA,s3://b/a.fq.gz\n', tsv)).toEqual({
      headers: ['Sample', 'R1'],
      records: [['A', 's3://b/a.fq.gz']],
    });
  });
});

describe('the column-matching step', () => {
  it('moves a file column to the sheet column chosen, unmapping it where it was', () => {
    const mapping = { sample: 0, fastq_1: 1, fastq_2: null, strandedness: null };
    expect(remapColumn(mapping, 'fastq_2', '1')).toEqual({
      sample: 0,
      fastq_1: null,
      fastq_2: 1,
      strandedness: null,
    });
    expect(remapColumn(mapping, 'sample', '')).toEqual({ ...mapping, sample: null });
    // The mapping it was given is left as it was.
    expect(mapping.fastq_1).toBe(1);
  });

  it('says what an import did and what it left out', () => {
    const headers = ['Sample', 'R1', 'R2', 'batch'];
    const all = { sample: 0, fastq_1: 1, fastq_2: 2, strandedness: null };
    expect(importNotes(spec, 'core.csv', headers, all, 3)).toBe(
      'Imported 3 rows from core.csv. Skipped columns this workflow does not use: batch.' +
        ' The file has no strandedness column.',
    );
    const idOnly = { sample: 0, fastq_1: null, fastq_2: null, strandedness: null };
    expect(importNotes(spec, 'core.csv', headers, idOnly, 1)).toBe(
      'Imported 1 row from core.csv. Skipped columns this workflow does not use: batch.' +
        ' Left out, as you chose: R1, R2. The file has no fastq_1, strandedness column.',
    );
  });
});
