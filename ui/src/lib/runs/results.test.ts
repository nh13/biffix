import { describe, expect, it } from 'vitest';
import type { RunReport } from '#lib/api/index.js';
import {
  formatLabel,
  noResultsReason,
  previewKind,
  resultCards,
  sampleCount,
  showsAuthorHint,
} from './results';

const report = (name: string, id: string, over: Partial<RunReport> = {}): RunReport => ({
  name,
  kind: 'declared',
  format: 'html',
  output: {
    id,
    name: `${id}.html`,
    path: `${id}.html`,
    s3_uri: `s3://b/${id}.html`,
    role: 'published',
    servable: true,
  },
  ...over,
});

describe('resultCards', () => {
  it("folds a per-sample result into one card, in the server's sample order", () => {
    const cards = resultCards([
      report('Gene counts', 'counts', { format: 'table' }),
      report('Per-sample QC', 'fqc-t10', { sample: 'Tumor_10', declaration: 1 }),
      report('Per-sample QC', 'fqc-t2', { sample: 'Tumor_2', declaration: 1 }),
      report('QC report', 'multiqc', { kind: 'multiqc' }),
    ]);
    expect(cards.map((c) => [c.name, c.perSample, c.entries.map((e) => e.sample)])).toEqual([
      ['Gene counts', false, [null]],
      ['Per-sample QC', true, ['Tumor_10', 'Tumor_2']],
      ['QC report', false, [null]],
    ]);
  });

  it('keeps two per-sample declarations that share a name as two cards, never one sample twice', () => {
    const cards = resultCards([
      report('QC', 'a1', { sample: 'S1', declaration: 0 }),
      report('QC', 'a2', { sample: 'S2', declaration: 0 }),
      report('QC', 'b1', { sample: 'S1', format: 'pdf', declaration: 1 }),
      report('QC', 'b2', { sample: 'S2', format: 'pdf', declaration: 1 }),
    ]);
    expect(cards.map((c) => [c.key, c.entries.map((e) => e.report.output.id)])).toEqual([
      ['name:QC', ['a1', 'a2']],
      ['name:QC:2', ['b1', 'b2']],
    ]);
    // Each entry keeps its own format.
    expect(cards[1].entries[0].report.format).toBe('pdf');
  });

  // A sample's files (a paired-end sample's R1 and R2), as the server lists them: together, in
  // path order, in sample order. Declaration 0's unless `over` says otherwise.
  const file = (name: string, sample: string, path: string, over: Partial<RunReport> = {}) =>
    report(name, path, {
      sample,
      declaration: 0,
      ...over,
      output: { ...report(name, path).output, path, s3_uri: `s3://b/out/${path}` },
    });

  it("keeps each sample's several files in its one card, and counts samples, not files", () => {
    const cards = resultCards([
      file('QC', 'S1', 'fastqc/S1_R1_fastqc.zip'),
      file('QC', 'S1', 'fastqc/S1_R2_fastqc.zip'),
      file('QC', 'S2', 'fastqc/S2_R1_fastqc.zip'),
      file('QC', 'S2', 'fastqc/S2_R2_fastqc.zip'),
    ]);
    expect(cards.map((c) => [c.key, c.entries.map((e) => e.report.output.id)])).toEqual([
      [
        'name:QC',
        [
          'fastqc/S1_R1_fastqc.zip',
          'fastqc/S1_R2_fastqc.zip',
          'fastqc/S2_R1_fastqc.zip',
          'fastqc/S2_R2_fastqc.zip',
        ],
      ],
    ]);
    expect(sampleCount(cards[0])).toBe(2);
  });

  it('keeps paired-end declarations that share a name as two cards', () => {
    const cards = resultCards([
      file('QC', 'S1', 'raw/S1_1.html'),
      file('QC', 'S1', 'raw/S1_2.html'),
      file('QC', 'S2', 'raw/S2_1.html'),
      file('QC', 'S2', 'raw/S2_2.html'),
      file('QC', 'S1', 'trimmed/S1_1.html', { declaration: 1 }),
      file('QC', 'S1', 'trimmed/S1_2.html', { declaration: 1 }),
      file('QC', 'S2', 'trimmed/S2_1.html', { declaration: 1 }),
    ]);
    expect(cards.map((c) => [c.key, sampleCount(c), c.entries.length])).toEqual([
      ['name:QC', 2, 4],
      ['name:QC:2', 2, 3],
    ]);
  });

  it("tells declarations apart by the server's declaration index, however their files sort", () => {
    // One declaration's last sample is the next one's first, its file before the other's in path
    // order.
    const cards = resultCards([
      file('QC', 'S1', 'qc_b/S1.html', { declaration: 0 }),
      file('QC', 'S1', 'qc_a/S1.html', { declaration: 1 }),
      file('QC', 'S2', 'qc_a/S2_1.html', { declaration: 1 }),
      file('QC', 'S2', 'qc_a/S2_2.html', { declaration: 1 }),
    ]);
    expect(cards.map((c) => [c.entries.length, sampleCount(c)])).toEqual([
      [1, 1],
      [3, 2],
    ]);
  });

  it('keeps a declared result apart from FastQC results found by name of the same name', () => {
    const cards = resultCards([
      file('Per-sample QC', 'S1', 'fastqc/S1_fastqc.html', { format: 'fastqc' }),
      file('Per-sample QC', 'S1', 'qc/S1_fastqc.zip', {
        format: 'fastqc',
        kind: 'detected',
        declaration: undefined,
      }),
      file('Per-sample QC', 'S2', 'qc/S2_fastqc.zip', {
        format: 'fastqc',
        kind: 'detected',
        declaration: undefined,
      }),
    ]);
    expect(cards.map((c) => [c.key, sampleCount(c)])).toEqual([
      ['name:Per-sample QC', 1],
      ['name:Per-sample QC:2', 2],
    ]);
  });

  it("lists a FastQC result once when both its HTML report and its zip are a sample's files", () => {
    const cards = resultCards([
      file('FastQC', 'S1', 'fastqc/S1_fastqc.html', { format: 'fastqc' }),
      file('FastQC', 'S1', 'fastqc/S1_fastqc.zip', { format: 'fastqc' }),
      file('FastQC', 'S2', 'fastqc/S2_fastqc.zip', { format: 'fastqc' }),
    ]);
    expect(cards[0].entries.map((e) => e.report.output.id)).toEqual([
      'fastqc/S1_fastqc.html',
      'fastqc/S2_fastqc.zip',
    ]);
  });

  it('keeps two reports of one name without a sample as two cards', () => {
    const cards = resultCards([report('Plot', 'a'), report('Plot', 'b')]);
    expect(cards.map((c) => c.key)).toEqual(['output:a', 'output:b']);
    expect(resultCards([])).toEqual([]);
  });
});

describe('formats', () => {
  it('labels each format, an unknown one as a file', () => {
    expect(formatLabel('html')).toBe('Report');
    expect(formatLabel('table')).toBe('Table');
    expect(formatLabel('alignment')).toBe('Alignments');
    expect(formatLabel('fastqc')).toBe('FastQC');
    expect(formatLabel('hologram')).toBe('File');
    expect(formatLabel('constructor')).toBe('File');
  });

  it('previews tables and images only', () => {
    expect(previewKind('table')).toBe('table');
    expect(previewKind('image')).toBe('image');
    expect(previewKind('html')).toBeNull();
    expect(previewKind('pdf')).toBeNull();
  });
});

describe('noResultsReason', () => {
  it('tells a version that declares no key results from one whose analysis wrote none', () => {
    expect(noResultsReason({ form_schema: { fields: [] } })).toBe('undeclared');
    expect(noResultsReason({ form_schema: { fields: [], reports: [] } })).toBe('undeclared');
    expect(noResultsReason({ form_schema: null })).toBe('undeclared');
    const declared = { form_schema: { reports: [{ name: 'QC', path: 'qc.html' }] } };
    expect(noResultsReason(declared)).toBe('unwritten');
  });

  it('cannot tell before the version loads', () => {
    expect(noResultsReason(null)).toBe('unknown');
  });
});

describe('showsAuthorHint', () => {
  it("is for the version's registrar only, when both ids are known", () => {
    expect(showsAuthorHint('bob', 'bob', 'lab')).toBe(true);
    expect(showsAuthorHint('bob', 'alice', 'lab')).toBe(false);
    expect(showsAuthorHint('bob', 'alice', 'expert')).toBe(false);
  });

  it('is for the Expert view only when the registrar cannot be told', () => {
    expect(showsAuthorHint(null, 'bob', 'lab')).toBe(false);
    expect(showsAuthorHint('bob', null, 'lab')).toBe(false);
    expect(showsAuthorHint(undefined, null, 'expert')).toBe(true);
  });
});
