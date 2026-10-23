// How a cost reads. A run's one cost, on the Lab analysis page (`analysis.ts` `costText`) and in
// both views' runs lists (`RunCostCell`), by one rule, the server's (`RunCost::priced_usd`): a run
// has a cost only once at least one of its jobs is priced. (The Expert run header and Cost tab word
// a run's cost in their own detail.) And a job's cost, in the Jobs table, its pane and the attempt
// picker. Pure, so it is unit-tested.

import type { RunDetail, RunSummary } from '#lib/api/index.js';
import { fmtUsd } from '#lib/util/format.js';
import { isLiveRun } from '#lib/runs/runState.js';

/** A run's cost as one number: its estimate once a job is priced, else null (nothing priced). */
export function pricedUsd(cost: RunDetail['cost'] | null | undefined): number | null {
  return cost && cost.priced_jobs > 0 ? cost.estimated_usd : null;
}

/** How far along a run's cost is: still running, or ended with jobs still to be priced. */
export interface CostProgress {
  /** The run is still running: its cost is an estimate so far. */
  live: boolean;
  /** Some of its jobs are not priced yet: an ended run's cost can still grow. */
  pending: boolean;
}

/** A cost as shown: its text, a tooltip, and what a screen reader says for it. */
export interface CostLine {
  text: string;
  title: string;
  sr: string;
}

/**
 * The run's cost `usd` (its priced estimate) as shown: `≈ $1.20 so far` while it runs,
 * `$13.80 + pending` once it ended with jobs still to be priced (the coordinator is priced only
 * after the run ends), `$13.80` once every job is priced.
 */
export function costLine(usd: number, { live, pending }: CostProgress): CostLine {
  const amount = fmtUsd(usd);
  if (live)
    return {
      text: `≈ ${amount} so far`,
      title: 'An estimate: still running',
      sr: `about ${amount} so far`,
    };
  if (pending)
    return {
      text: `${amount} + pending`,
      title: 'Some of its machines are not priced yet, so this can still grow',
      sr: `${amount} so far; some machines not priced yet`,
    };
  return { text: amount, title: 'Estimated from AWS prices for the machines it used', sr: amount };
}

/** What shows instead of a cost when nothing is priced: a dash, and why. */
export function noCostLine({ live, pending }: CostProgress): CostLine {
  const why = live || pending ? 'No cost yet' : 'No cost recorded';
  return { text: '—', title: why, sr: why };
}

/** A run's cost in a list row (`RunSummary.cost_usd` / `cost_pending`), as its run page has it. */
export function listCostLine(
  run: Pick<RunSummary, 'cost_usd' | 'cost_pending' | 'terminal'>,
): CostLine {
  const progress = { live: isLiveRun(run), pending: run.cost_pending === true };
  return run.cost_usd == null ? noCostLine(progress) : costLine(run.cost_usd, progress);
}

/** A job's cost, an estimate (`~`) unless it is below the smallest shown; blank when unpriced. */
export function jobCostText(usd: number | null | undefined): string {
  if (usd == null) return '';
  const s = fmtUsd(usd);
  return `${s.startsWith('<') ? '' : '~'}${s}`;
}
