// The launch footer's estimate: what a run of this many samples usually costs and how long it
// takes, from the workflow's recent successful runs.

import type { WorkflowEstimate } from '$lib/api';
import { plural } from '$lib/util/format';

export interface EstimateFacts {
  usd_low: number;
  usd_high: number;
  usd_median: number;
  hours_median: number;
  runs: number;
}

/**
 * A USD amount as estimates round it: cents below $1, one decimal below $10, whole dollars above.
 * The band is chosen after rounding, so $0.996 reads `$1` (not `$1.00`) and $9.96 `$10`.
 */
export function fmtEstimateUsd(v: number): string {
  const cents = Math.round(v * 100) / 100;
  if (cents === 0) return v > 0 ? '<$0.01' : '$0.00';
  if (cents < 1) return `$${cents.toFixed(2)}`;
  const tenths = Math.round(v * 10) / 10;
  if (tenths < 10) return `$${tenths.toFixed(1).replace(/\.0$/, '')}`;
  return `$${Math.round(v).toLocaleString('en-US')}`;
}

/** Hours as estimates say them: `40 min`, `2.5 h`, `12 h`; the unit chosen after rounding. */
export function fmtHours(hours: number): string {
  const minutes = Math.round(hours * 60);
  if (minutes < 60) return `${Math.max(1, minutes)} min`;
  const tenths = Math.round(hours * 10) / 10;
  return `${tenths < 10 ? tenths.toFixed(1).replace(/\.0$/, '') : Math.round(hours)} h`;
}

/** `About $12–$18 for 6 samples · usually ~3 h · from 5 runs`. */
export function describeEstimate(e: EstimateFacts, samples: number): string {
  const lo = fmtEstimateUsd(e.usd_low);
  const hi = fmtEstimateUsd(e.usd_high);
  const cost = lo === hi ? `About ${lo}` : `About ${lo}–${hi}`;
  return `${cost} for ${plural(samples, 'sample')} · usually ~${fmtHours(e.hours_median)} · from ${plural(e.runs, 'run')}`;
}

/** Where the estimate stands: worked out (`text`), still being asked for, or the ask failed. */
export interface EstimateState {
  text: string | null;
  estimating: boolean;
  failed: boolean;
  /** The samples listed (none: no estimate can be worked out). */
  samples: number;
}

/** The wizard's Review line about the cost: the estimate, or why there is none. */
export function estimateLine(s: EstimateState): string {
  if (s.text) return `${s.text}. An estimate, not a limit.`;
  if (s.estimating) return 'Estimating the cost…';
  if (s.failed) return "Couldn't get a cost estimate just now.";
  if (s.samples === 0)
    return 'No cost estimate: one is worked out from the samples listed on Add samples.';
  return 'No cost estimate yet: it appears once this pipeline has finished analyses of samples to go by.';
}

/** `Usually ~2.5 h · about $14 · from 6 analyses`: a catalog card's typical run; null without
 * one. */
export function typicalLine(
  typical: WorkflowEstimate['typical'] | null | undefined,
): string | null {
  if (!typical || !(typical.hours_median > 0) || typical.runs <= 0) return null;
  const cost =
    typical.usd_median != null && Number.isFinite(typical.usd_median)
      ? ` · about ${fmtEstimateUsd(typical.usd_median)}`
      : '';
  return `Usually ~${fmtHours(typical.hours_median)}${cost} · from ${plural(typical.runs, 'analysis', 'analyses')}`;
}
