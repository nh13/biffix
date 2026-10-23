import { describe, expect, it } from 'vitest';
import type { FastqcStatusSample } from '$lib/api';
import { basicStatRows, samePick, statusChoices, statusSections } from './fastqcStatus';

const result = (fastqc_name: string, sample?: string): FastqcStatusSample => ({
  fastqc_name,
  ...(sample ? { sample } : {}),
  modules: [],
  stats: {},
});

describe('statusChoices', () => {
  it('names each by its sample, and by file too when a sample has several', () => {
    const got = statusChoices([
      result('S1_R1', 'S1'),
      result('S1_R2', 'S1'),
      result('S10_R1', 'S10'),
      result('stray'),
    ]);
    expect(got.map((c) => c.label)).toEqual(['S1 · S1_R1', 'S1 · S1_R2', 'S10', 'stray']);
    expect(got[2].result.fastqc_name).toBe('S10_R1');
  });
});

describe('statusSections', () => {
  const inSection = (section: string | undefined, name: string, sample?: string) => ({
    ...result(name, sample),
    ...(section ? { section } : {}),
  });

  it('keeps each section apart, in the server order, the unanchored one as MultiQC names it', () => {
    const got = statusSections([
      inSection(undefined, 'S1_R1', 'S1'),
      inSection('fastqc_trimmed', 'S1_R1', 'S1'),
      inSection('fastqc_trimmed', 'S1_R2', 'S1'),
    ]);
    expect(got.map((s) => [s.section, s.label, s.choices.map((c) => c.label)])).toEqual([
      [null, 'fastqc', ['S1']],
      ['fastqc_trimmed', 'fastqc_trimmed', ['S1 · S1_R1', 'S1 · S1_R2']],
    ]);
  });

  it('is one section for a report that ran FastQC once, and none for no results', () => {
    expect(statusSections([result('S1_R1', 'S1')]).map((s) => s.section)).toEqual([null]);
    expect(statusSections([])).toEqual([]);
  });

  it('copes with repeated MultiQC names', () => {
    const got = statusSections([result('S1', 'S1'), result('S1', 'S1')]);
    expect(got[0].choices.map((c) => c.result.fastqc_name)).toEqual(['S1', 'S1']);
  });
});

describe('samePick', () => {
  const choices = (...r: [string, string?][]) => statusChoices(r.map(([n, s]) => result(n, s)));
  const raw = choices(['A_R1', 'A'], ['B_R1', 'B'], ['C_R1', 'C']);

  it("keeps the same result across sections by identity, never by a neighbour's place", () => {
    // Trimming left B with no reads: the trimmed section has one result fewer.
    const trimmed = choices(['A_R1', 'A'], ['C_R1', 'C']);
    expect(samePick(raw, 2, trimmed)).toBe(1);
    // A result trimming renamed is found by its sample.
    const renamed = choices(['A_val_1', 'A'], ['C_val_1', 'C']);
    expect(samePick(raw, 2, renamed)).toBe(1);
  });

  it("finds a sample's file by its place among that sample's files, else the first choice", () => {
    const pairs = choices(['A_R1', 'A'], ['A_R2', 'A']);
    const trimmedPairs = choices(['A_val_1', 'A'], ['A_val_2', 'A']);
    expect(samePick(pairs, 1, trimmedPairs)).toBe(1);
    expect(samePick(raw, 1, choices(['A_R1', 'A']))).toBe(0);
    expect(samePick([], 0, raw)).toBe(0);
  });
});

describe('basicStatRows', () => {
  it('formats what MultiQC recorded and leaves out what it did not', () => {
    expect(
      basicStatRows({
        total_sequences: 1234567,
        total_bases: '150 kbp',
        sequence_length: '35-151',
        percent_gc: 50,
        percent_duplicates: 12.345,
        poor_quality: 0,
      }),
    ).toEqual([
      ['Total sequences', '1,234,567'],
      ['Total bases', '150 kbp'],
      ['Sequence length', '35-151 bp'],
      ['GC', '50%'],
      ['Duplicates', '12.3%'],
      ['Flagged poor quality', '0'],
    ]);
    expect(basicStatRows({ percent_gc: 41.25 })).toEqual([['GC', '41.3%']]);
    expect(basicStatRows({})).toEqual([]);
  });
});
