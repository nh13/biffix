// The FastQC viewer's data: a module's pass / warn / fail, which picture it gets, and
// the geometry of its plots, from the pipeline's own `fastqc_data.txt` (`GET …/fastqc`). Pure, so
// the plots are unit-tested; the component only draws what these return.

import type { FastqcModule } from '#lib/api/index.js';

/** How a module's status reads: a symbol and a word (never colour alone), and its tone. */
export interface StatusText {
  tone: 'pass' | 'warn' | 'fail' | 'other';
  icon: string;
  label: string;
}

/** A module's status as shown; one FastQC does not write (the field is open) reads as itself. */
export function statusOf(status: string): StatusText {
  switch (status.toLowerCase()) {
    case 'pass':
      return { tone: 'pass', icon: '✓', label: 'Pass' };
    case 'warn':
      return { tone: 'warn', icon: '!', label: 'Warning' };
    case 'fail':
      return { tone: 'fail', icon: '✕', label: 'Fail' };
    default:
      return { tone: 'other', icon: '?', label: status || 'Unknown' };
  }
}

/** How many modules have each status (the viewer's summary line). */
export function statusCounts(
  modules: readonly Pick<FastqcModule, 'status'>[],
): Record<StatusText['tone'], number> {
  const counts = { pass: 0, warn: 0, fail: 0, other: 0 };
  for (const m of modules) counts[statusOf(m.status).tone] += 1;
  return counts;
}

/**
 * `1 failed · 2 with a warning · 7 passed`, as the FastQC viewer says it; a status other than those
 * three (FastQC's statuses are an open set) is counted too, so the summary covers every module.
 */
export function statusSummary(modules: readonly { status: string }[]): string {
  const counts = statusCounts(modules);
  return [
    counts.fail ? `${counts.fail} failed` : '',
    counts.warn ? `${counts.warn} with a warning` : '',
    counts.pass ? `${counts.pass} passed` : '',
    counts.other ? `${counts.other} with another status` : '',
  ]
    .filter(Boolean)
    .join(' · ');
}

/**
 * Which picture a module gets: `quality` (Per base sequence quality's box plot), `lines` (a table
 * whose value columns are all numbers: one line per column over the first column's rows), or
 * `table` (anything else, including a module this viewer has never seen).
 */
export type ModuleView = 'quality' | 'lines' | 'table';

/** Modules drawn as a table even though their cells are numbers (a line plot would mislead). */
const TABLE_MODULES = new Set(['basic statistics', 'per tile sequence quality', 'kmer content']);

export function viewOf(m: FastqcModule): ModuleView {
  if (qualityBoxes(m)) return 'quality';
  if (TABLE_MODULES.has(m.name.toLowerCase())) return 'table';
  return lineData(m) ? 'lines' : 'table';
}

/** A cell as a number: `null` for FastQC's `NaN` and anything that is not a number. */
function numberOf(cell: string | undefined): number | null {
  if (cell == null || cell.trim() === '') return null;
  const n = Number(cell);
  return Number.isFinite(n) ? n : null;
}

/** Whether a cell is a number, or FastQC's `NaN` (a value it could not compute). */
const numeric = (cell: string | undefined) =>
  cell != null && (numberOf(cell) !== null || cell.trim() === 'NaN');

// ------------------------------------------------------------------------------------------------
// Per base sequence quality: one box per base (or base range).

/** One base (or range of bases) of Per base sequence quality, as FastQC reports it. */
export interface QualityBox {
  /** The base or range (`1`, `10-14`). */
  label: string;
  mean: number;
  median: number;
  lower: number;
  upper: number;
  p10: number;
  p90: number;
}

const QUALITY_COLUMNS = [
  'Mean',
  'Median',
  'Lower Quartile',
  'Upper Quartile',
  '10th Percentile',
  '90th Percentile',
] as const;

/**
 * The boxes of a Per base sequence quality module, read by column name; `null` when it is not that
 * module or a column is missing. A row with a value that is not a number is left out.
 */
export function qualityBoxes(m: FastqcModule): QualityBox[] | null {
  if (m.name.toLowerCase() !== 'per base sequence quality') return null;
  const at = QUALITY_COLUMNS.map((c) => m.columns.indexOf(c));
  if (at.some((i) => i < 0)) return null;
  const boxes: QualityBox[] = [];
  for (const row of m.rows) {
    const values = at.map((i) => numberOf(row[i]));
    if (!values.every((v): v is number => v !== null)) continue;
    const [mean, median, lower, upper, p10, p90] = values;
    boxes.push({ label: row[0] ?? '', mean, median, lower, upper, p10, p90 });
  }
  return boxes.length > 0 ? boxes : null;
}

/** Where a plot draws: the SVG's size and the margins around its plotting area. */
export interface PlotFrame {
  width: number;
  height: number;
  left: number;
  right: number;
  top: number;
  bottom: number;
}

export const DEFAULT_FRAME: PlotFrame = {
  width: 640,
  height: 280,
  left: 44,
  right: 12,
  top: 10,
  bottom: 36,
};

export interface Tick {
  /** The position along its axis, in SVG units. */
  at: number;
  label: string;
}

/** The background bands of the quality plot: FastQC's good (≥ 28), so-so (20–28) and poor
 * (< 20). */
export interface QualityBand {
  y: number;
  height: number;
  tone: 'good' | 'fair' | 'poor';
}

export interface QualityPlot {
  yMax: number;
  bands: QualityBand[];
  boxes: {
    label: string;
    /** The box's left edge and width. */
    x: number;
    width: number;
    /** Its centre (the whiskers and the mean's line). */
    cx: number;
    yUpper: number;
    yLower: number;
    yMedian: number;
    yP10: number;
    yP90: number;
    yMean: number;
  }[];
  /** The mean across bases, as an SVG path. */
  meanPath: string;
  xTicks: Tick[];
  yTicks: Tick[];
}

/** A value's y in the plotting area, for an axis from 0 to `yMax`. */
const yOf = (f: PlotFrame, yMax: number, v: number) =>
  f.top + (1 - Math.min(Math.max(v, 0), yMax) / yMax) * (f.height - f.top - f.bottom);

/** At most `max` evenly spaced labels along `n` categories, the first always among them. */
function categoryTicks(labels: readonly string[], xOf: (i: number) => number, max = 10): Tick[] {
  const step = Math.max(1, Math.ceil(labels.length / max));
  const ticks: Tick[] = [];
  for (let i = 0; i < labels.length; i += step) ticks.push({ at: xOf(i), label: labels[i] });
  return ticks;
}

/** A tick step near a fifth of `max`: 1, 2, 2.5 or 5 times a power of ten. */
export function niceStep(max: number): number {
  if (!(max > 0)) return 1;
  const rough = max / 5;
  const power = 10 ** Math.floor(Math.log10(rough));
  const step = [1, 2, 2.5, 5, 10].find((m) => m * power >= rough) ?? 10;
  return step * power;
}

/** The decimals a `niceStep` needs written out: 0.00025 needs 5, 2.5 needs 1, 10 needs none. */
function stepDecimals(step: number): number {
  const power = Math.floor(Math.log10(step));
  const mantissa = Math.round((step / 10 ** power) * 10) / 10;
  return Math.max(0, -power + (Number.isInteger(mantissa) ? 0 : 1));
}

/**
 * The y axis for values up to `max`: a top at a whole number of steps, and its ticks, each written
 * to the step's precision (FastQC's N and adapter content are often a few hundredths of a percent).
 */
function valueAxis(f: PlotFrame, max: number): { yMax: number; yTicks: Tick[] } {
  const step = niceStep(max);
  const n = Math.max(1, Math.ceil(max / step - 1e-9));
  const yMax = n * step;
  const decimals = stepDecimals(step);
  const yTicks: Tick[] = [];
  for (let i = 0; i <= n; i++)
    yTicks.push({ at: yOf(f, yMax, i * step), label: (i * step).toFixed(decimals) });
  return { yMax, yTicks };
}

/** The geometry of the quality box plot (y from 0 to at least 40, FastQC's usual top). */
export function qualityPlot(
  boxes: readonly QualityBox[],
  f: PlotFrame = DEFAULT_FRAME,
): QualityPlot {
  const { yMax, yTicks } = valueAxis(f, Math.max(40, ...boxes.map((b) => b.p90)));
  const plotWidth = f.width - f.left - f.right;
  const slot = plotWidth / Math.max(1, boxes.length);
  const width = Math.max(1, slot * 0.7);
  const cxOf = (i: number) => f.left + slot * (i + 0.5);
  const y = (v: number) => yOf(f, yMax, v);
  const band = (from: number, to: number, tone: QualityBand['tone']): QualityBand => ({
    y: y(Math.min(to, yMax)),
    height: y(from) - y(Math.min(to, yMax)),
    tone,
  });
  const placed = boxes.map((b, i) => ({
    label: b.label,
    x: cxOf(i) - width / 2,
    width,
    cx: cxOf(i),
    yUpper: y(b.upper),
    yLower: y(b.lower),
    yMedian: y(b.median),
    yP10: y(b.p10),
    yP90: y(b.p90),
    yMean: y(b.mean),
  }));
  return {
    yMax,
    bands: [band(28, yMax, 'good'), band(20, 28, 'fair'), band(0, 20, 'poor')],
    boxes: placed,
    meanPath: placed.map((b, i) => `${i === 0 ? 'M' : 'L'}${fmt(b.cx)} ${fmt(b.yMean)}`).join(' '),
    xTicks: categoryTicks(
      boxes.map((b) => b.label),
      cxOf,
    ),
    yTicks,
  };
}

/** A coordinate, rounded for a compact path. */
const fmt = (n: number) => String(Math.round(n * 100) / 100);

// ------------------------------------------------------------------------------------------------
// Line plots: every other numeric module (quality scores, base content, GC, N content, lengths,
// duplication, adapters).

export interface LineData {
  /** The first column's values: the x categories, in the file's order. */
  labels: string[];
  /** The first column's name (the x axis). */
  xName: string;
  /** One series per other column; `null` where FastQC wrote `NaN`. */
  series: { name: string; values: (number | null)[] }[];
}

/**
 * A module as lines: its first column the x categories, every other column a series. `null` unless
 * it has at least two rows and every value cell is a number (or `NaN`), so a table of sequences or
 * sources stays a table.
 */
export function lineData(m: FastqcModule): LineData | null {
  if (m.columns.length < 2 || m.rows.length < 2) return null;
  const valueCells = m.rows.flatMap((r) => m.columns.slice(1).map((_, j) => r[j + 1]));
  if (!valueCells.every(numeric) || !valueCells.some((c) => numberOf(c) !== null)) return null;
  return {
    labels: m.rows.map((r) => r[0] ?? ''),
    xName: m.columns[0],
    series: m.columns.slice(1).map((name, j) => ({
      name,
      values: m.rows.map((r) => numberOf(r[j + 1])),
    })),
  };
}

export interface LinePlot {
  yMax: number;
  /** One path per series, in the series' order (a gap where a value is missing). */
  paths: { name: string; d: string }[];
  xTicks: Tick[];
  yTicks: Tick[];
}

/** The geometry of a line plot: categories evenly spaced (as FastQC draws grouped bases). */
export function linePlot(data: LineData, f: PlotFrame = DEFAULT_FRAME): LinePlot {
  const max = Math.max(
    0,
    ...data.series.flatMap((s) => s.values.filter((v): v is number => v !== null)),
  );
  const { yMax, yTicks } = valueAxis(f, max);
  const plotWidth = f.width - f.left - f.right;
  const n = data.labels.length;
  const xOf = (i: number) => f.left + (n <= 1 ? plotWidth / 2 : (plotWidth * i) / (n - 1));
  const paths = data.series.map((s) => {
    let d = '';
    let drawing = false;
    s.values.forEach((v, i) => {
      if (v === null) {
        drawing = false;
        return;
      }
      d += `${d ? ' ' : ''}${drawing ? 'L' : 'M'}${fmt(xOf(i))} ${fmt(yOf(f, yMax, v))}`;
      drawing = true;
    });
    return { name: s.name, d };
  });
  return { yMax, paths, xTicks: categoryTicks(data.labels, xOf), yTicks };
}

/** The `[key, value]` header lines a module carries before its table (`Total Deduplicated …`). */
export function extrasOf(m: FastqcModule): [string, string][] {
  return (m.extra ?? []).filter((kv) => kv.length >= 2).map((kv) => [kv[0], kv[1]]);
}
