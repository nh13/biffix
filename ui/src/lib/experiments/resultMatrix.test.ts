import { describe, expect, it } from 'vitest';
import type { RunReport } from '#lib/api/index.js';
import {
  cardFor,
  forSample,
  initialView,
  kindGroups,
  resultSamples,
  type RunCards,
} from './resultMatrix';
import { resultCards } from '#lib/runs/results.js';

let id = 0;
const report = (name: string, format: string, sample: string | null = null): RunReport =>
  ({
    name,
    kind: 'declared',
    format,
    ...(sample ? { sample } : {}),
    output: {
      id: `o${++id}`,
      name: `${name}.${format}`,
      path: `${id}/${name}.${format}`,
      s3_uri: `s3://b/${id}/${name}`,
    },
  }) as unknown as RunReport;

const run = (runId: string, reports: RunReport[]): RunCards => ({
  runId,
  cards: resultCards(reports),
});

const batch10 = run('r10', [
  report('QC report', 'html'),
  report('Gene counts', 'table'),
  report('Per-sample QC', 'fastqc', 'Normal_1'),
  report('Per-sample QC', 'fastqc', 'Tumor_1'),
  report('Alignments', 'alignment', 'Normal_1'),
]);
const batch12 = run('r12', [
  report('Gene counts', 'table'),
  report('PCA', 'image'),
  report('Per-sample QC', 'fastqc', 'Normal_2'),
  report('Per-sample QC', 'fastqc', 'Normal_1'),
]);

describe('kindGroups', () => {
  it('lists each result once, grouped by kind in reading order', () => {
    const groups = kindGroups([batch10, batch12]);
    expect(groups.map((g) => [g.label, g.columns.map((c) => c.name)])).toEqual([
      ['Report', ['QC report']],
      ['Table', ['Gene counts']],
      ['Image', ['PCA']],
      ['FastQC', ['Per-sample QC']],
      ['Alignments', ['Alignments']],
    ]);
    expect(groups[3].columns[0].perSample).toBe(true);
  });

  it('is empty without results, and puts an unknown kind last as a file', () => {
    expect(kindGroups([])).toEqual([]);
    const groups = kindGroups([run('x', [report('Odd', 'weird'), report('Plot', 'image')])]);
    expect(groups.map((g) => g.label)).toEqual(['Image', 'File']);
  });
});

describe('cardFor / forSample', () => {
  it("finds an analysis's result, and one sample's files of a per-sample result", () => {
    expect(cardFor(batch12.cards, 'QC report', null)).toBeNull();
    const qc = cardFor(batch10.cards, 'Per-sample QC', 'Tumor_1')!;
    expect(qc.entries.map((e) => e.sample)).toEqual(['Tumor_1']);
    // A sample this analysis did not have: nothing to open.
    expect(cardFor(batch10.cards, 'Per-sample QC', 'Normal_2')).toBeNull();
    // A whole-study result is the same whatever the sample.
    const counts = cardFor(batch10.cards, 'Gene counts', 'Tumor_1')!;
    expect(counts.entries).toHaveLength(1);
    expect(forSample(qc, null)).toBe(qc);
  });

  it('stands for every card of a name, not only the first (a glob of plots)', () => {
    const plots = run('r20', [report('Plots', 'image'), report('Plots', 'image')]);
    expect(plots.cards.filter((c) => c.name === 'Plots')).toHaveLength(2);
    expect(cardFor(plots.cards, 'Plots', null)!.entries).toHaveLength(2);
  });
});

describe('resultSamples', () => {
  it('names every sample of the per-sample results, in natural order', () => {
    expect(resultSamples([batch10, batch12])).toEqual(['Normal_1', 'Normal_2', 'Tumor_1']);
    expect(
      resultSamples([run('y', [report('S10', 'fastqc', 'S10'), report('S10', 'fastqc', 'S2')])]),
    ).toEqual(['S2', 'S10']);
  });
});

describe('initialView', () => {
  it('starts as remembered, else a table once two analyses have results', () => {
    expect(initialView('result', 5)).toBe('result');
    expect(initialView(null, 2)).toBe('table');
    expect(initialView(null, 1)).toBe('analysis');
    expect(initialView('bogus', 3)).toBe('table');
  });
});
