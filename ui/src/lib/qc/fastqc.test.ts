import { describe, expect, it } from 'vitest';
import type { FastqcModule } from '$lib/api';
import {
  DEFAULT_FRAME as F,
  extrasOf,
  lineData,
  linePlot,
  niceStep,
  qualityBoxes,
  qualityPlot,
  statusCounts,
  statusOf,
  statusSummary,
  viewOf,
} from './fastqc';

const mod = (over: Partial<FastqcModule> & Pick<FastqcModule, 'name'>): FastqcModule => ({
  status: 'pass',
  columns: [],
  rows: [],
  ...over,
});

const QUALITY_COLUMNS = [
  'Base',
  'Mean',
  'Median',
  'Lower Quartile',
  'Upper Quartile',
  '10th Percentile',
  '90th Percentile',
];
/** Per base sequence quality over `n` bases, quality falling along the read. */
const quality = (n: number, top = 38): FastqcModule =>
  mod({
    name: 'Per base sequence quality',
    status: 'fail',
    columns: QUALITY_COLUMNS,
    rows: Array.from({ length: n }, (_, i) => {
      const q = top - i;
      return [String(i + 1), `${q}`, `${q}`, `${q - 2}`, `${q + 1}`, `${q - 5}`, `${q + 2}`];
    }),
  });

describe('statuses', () => {
  it('reads pass, warn and fail as a symbol and a word, and any other as itself', () => {
    expect(statusOf('pass')).toEqual({ tone: 'pass', icon: '✓', label: 'Pass' });
    expect(statusOf('WARN')).toMatchObject({ tone: 'warn', label: 'Warning' });
    expect(statusOf('fail')).toMatchObject({ tone: 'fail', icon: '✕', label: 'Fail' });
    expect(statusOf('skipped')).toEqual({ tone: 'other', icon: '?', label: 'skipped' });
    expect(statusOf('')).toMatchObject({ tone: 'other', label: 'Unknown' });
  });

  it('counts the modules by status', () => {
    const modules = ['pass', 'pass', 'warn', 'fail', 'new'].map((status, i) =>
      mod({ name: `m${i}`, status }),
    );
    expect(statusCounts(modules)).toEqual({ pass: 2, warn: 1, fail: 1, other: 1 });
  });
});

describe('viewOf', () => {
  it('draws per-base quality as boxes, numeric tables as lines, the rest as tables', () => {
    expect(viewOf(quality(5))).toBe('quality');
    const gc = mod({
      name: 'Per sequence GC content',
      columns: ['GC Content', 'Count'],
      rows: [
        ['0', '1.0'],
        ['1', '3.5'],
      ],
    });
    expect(viewOf(gc)).toBe('lines');
    const basic = mod({
      name: 'Basic Statistics',
      columns: ['Measure', 'Value'],
      rows: [
        ['Total Sequences', '2000'],
        ['Sequence length', '100'],
      ],
    });
    expect(viewOf(basic)).toBe('table');
    // A list of k-mers, not a series, even when every position is a single base.
    const kmers = mod({
      name: 'Kmer Content',
      columns: ['Sequence', 'Count', 'PValue', 'Obs/Exp Max', 'Max Obs/Exp Position'],
      rows: [
        ['GGGGG', '120', '0.0', '8.5', '1'],
        ['CCCCA', '80', '1e-4', '6.2', '3'],
      ],
    });
    expect(viewOf(kmers)).toBe('table');
    const tiles = mod({
      name: 'Per tile sequence quality',
      columns: ['Tile', 'Base', 'Mean'],
      rows: [
        ['1101', '1', '0.1'],
        ['1101', '2', '-0.2'],
      ],
    });
    expect(viewOf(tiles)).toBe('table');
    const overrepresented = mod({
      name: 'Overrepresented sequences',
      columns: ['Sequence', 'Count', 'Percentage', 'Possible Source'],
      rows: [
        ['ACGT', '12', '0.6', 'No Hit'],
        ['TTTT', '10', '0.5', 'No Hit'],
      ],
    });
    expect(viewOf(overrepresented)).toBe('table');
  });

  it('shows a module it has never seen by its cells: lines when numeric, else a table', () => {
    const numeric = mod({
      name: 'Per read wobble (FastQC 2)',
      columns: ['Position', 'Wobble'],
      rows: [
        ['1', '0.1'],
        ['2-3', '0.2'],
      ],
    });
    expect(viewOf(numeric)).toBe('lines');
    expect(
      viewOf(mod({ name: 'Something new', columns: ['Kind', 'Note'], rows: [['a', 'b']] })),
    ).toBe('table');
    // A Per base sequence quality without its columns is not a box plot.
    expect(
      qualityBoxes(mod({ name: 'Per base sequence quality', columns: ['Base', 'Mean'] })),
    ).toBe(null);
  });
});

describe('qualityPlot', () => {
  it('places one box per base, evenly, inside the plotting area', () => {
    const plot = qualityPlot(qualityBoxes(quality(10))!);
    expect(plot.boxes).toHaveLength(10);
    const slot = (F.width - F.left - F.right) / 10;
    plot.boxes.forEach((b, i) => {
      expect(b.cx).toBeCloseTo(F.left + slot * (i + 0.5));
      expect(b.x).toBeGreaterThanOrEqual(F.left);
      expect(b.x + b.width).toBeLessThanOrEqual(F.width - F.right);
      // Higher quality is higher up (smaller y): the box spans the quartiles, whiskers wider.
      expect(b.yUpper).toBeLessThan(b.yLower);
      expect(b.yP90).toBeLessThanOrEqual(b.yUpper);
      expect(b.yP10).toBeGreaterThanOrEqual(b.yLower);
    });
    expect(plot.meanPath.startsWith('M')).toBe(true);
    expect(plot.meanPath.split('L')).toHaveLength(10);
  });

  it('runs the axis to at least 40, with the good, fair and poor bands stacked to fill it', () => {
    const plot = qualityPlot(qualityBoxes(quality(3, 30))!);
    expect(plot.yMax).toBe(40);
    const bottom = F.height - F.bottom;
    const [good, fair, poor] = plot.bands;
    expect([good.tone, fair.tone, poor.tone]).toEqual(['good', 'fair', 'poor']);
    expect(good.y).toBeCloseTo(F.top);
    expect(good.y + good.height).toBeCloseTo(fair.y);
    expect(fair.y + fair.height).toBeCloseTo(poor.y);
    expect(poor.y + poor.height).toBeCloseTo(bottom);
    expect(plot.yTicks[0]).toEqual({ at: bottom, label: '0' });
    expect(plot.yTicks.at(-1)!.label).toBe('40');
    // A higher quality raises the top.
    expect(qualityPlot(qualityBoxes(quality(3, 45))!).yMax).toBeGreaterThan(40);
  });

  it('labels at most ten bases along x, starting at the first', () => {
    const plot = qualityPlot(qualityBoxes(quality(55))!);
    expect(plot.xTicks.length).toBeLessThanOrEqual(10);
    expect(plot.xTicks[0].label).toBe('1');
  });
});

describe('lineData and linePlot', () => {
  const content = mod({
    name: 'Per base sequence content',
    columns: ['Base', 'G', 'A', 'T', 'C'],
    rows: [
      ['1', '20', '30', '25', '25'],
      ['2', '21', '29', 'NaN', '25'],
      ['3-4', '22', '28', '24', '26'],
    ],
  });

  it('reads one series per value column, FastQC NaN as a gap', () => {
    const data = lineData(content)!;
    expect(data.labels).toEqual(['1', '2', '3-4']);
    expect(data.xName).toBe('Base');
    expect(data.series.map((s) => s.name)).toEqual(['G', 'A', 'T', 'C']);
    expect(data.series[2].values).toEqual([25, null, 24]);
  });

  it('draws each series across the plotting area, breaking the line at a gap', () => {
    const plot = linePlot(lineData(content)!);
    expect(plot.paths.map((p) => p.name)).toEqual(['G', 'A', 'T', 'C']);
    // From the left edge of the plotting area to its right edge.
    expect(plot.paths[0].d).toMatch(new RegExp(`^M${F.left} [\\d.]+ L`));
    expect(plot.paths[0].d).toMatch(new RegExp(`L${F.width - F.right} [\\d.]+$`));
    // T has a gap at base 2: two separate moves.
    expect(plot.paths[2].d.match(/M/g)).toHaveLength(2);
    expect(plot.paths[2].d).not.toContain('L');
    expect(plot.yMax).toBeGreaterThanOrEqual(30);
  });

  it('labels a small axis to its step, so no two ticks read the same', () => {
    const series = (...values: number[]) =>
      linePlot({ labels: values.map(String), xName: 'Base', series: [{ name: 'N', values }] });
    // Per base N content: a few hundredths of a percent.
    expect(series(0, 0.0012).yTicks.map((t) => t.label)).toEqual([
      '0.00000',
      '0.00025',
      '0.00050',
      '0.00075',
      '0.00100',
      '0.00125',
    ]);
    expect(series(0, 0.02).yTicks.map((t) => t.label)).toEqual([
      '0.000',
      '0.005',
      '0.010',
      '0.015',
      '0.020',
    ]);
    expect(series(0, 12).yTicks.map((t) => t.label)).toEqual([
      '0.0',
      '2.5',
      '5.0',
      '7.5',
      '10.0',
      '12.5',
    ]);
    expect(series(0, 40).yTicks.map((t) => t.label)).toEqual(['0', '10', '20', '30', '40']);
  });

  it('is not lines for a single row, a text column or no numbers at all', () => {
    expect(lineData(mod({ name: 'x', columns: ['a', 'b'], rows: [['1', '2']] }))).toBeNull();
    expect(
      lineData(
        mod({
          name: 'x',
          columns: ['a', 'b'],
          rows: [
            ['1', 'x'],
            ['2', 'y'],
          ],
        }),
      ),
    ).toBeNull();
    expect(
      lineData(
        mod({
          name: 'x',
          columns: ['a', 'b'],
          rows: [
            ['1', 'NaN'],
            ['2', 'NaN'],
          ],
        }),
      ),
    ).toBeNull();
  });
});

describe('niceStep and extras', () => {
  it('steps by 1, 2, 2.5 or 5 times a power of ten', () => {
    expect(niceStep(40)).toBe(10);
    expect(niceStep(9)).toBe(2);
    expect(niceStep(12)).toBe(2.5);
    expect(niceStep(0.7)).toBe(0.2);
    expect(niceStep(0)).toBe(1);
  });

  it('keeps the key/value lines a module carries before its table', () => {
    const dup = mod({
      name: 'Sequence Duplication Levels',
      extra: [['Total Deduplicated Percentage', '91.8'], ['odd']],
    });
    expect(extrasOf(dup)).toEqual([['Total Deduplicated Percentage', '91.8']]);
    expect(extrasOf(mod({ name: 'x' }))).toEqual([]);
  });
});

describe('statusSummary', () => {
  it('counts the statuses as the FastQC viewer words them', () => {
    const modules = ['fail', 'warn', 'WARN', 'pass', 'pass', 'pass'].map((status) => ({
      status,
    }));
    expect(statusSummary(modules)).toBe('1 failed · 2 with a warning · 3 passed');
    // A status FastQC added later is counted too, so the summary accounts for every module.
    expect(statusSummary([{ status: 'pass' }, { status: 'not run' }, { status: '' }])).toBe(
      '1 passed · 2 with another status',
    );
    expect(statusSummary([{ status: 'pass' }])).toBe('1 passed');
    expect(statusSummary([])).toBe('');
  });
});
