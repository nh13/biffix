// The Lab analysis page's layout and headline facts: which sections it shows, in
// what order for the run's state, the progress bar's fraction, who started it, and its one cost.
// Pure, so it is unit-tested.

import type { RunDetail } from '#lib/api/index.js';
import { costLine, pricedUsd } from '#lib/runs/cost.js';
import { fmtDate, fmtRelative } from '#lib/util/format.js';

export type SectionId = 'wrong' | 'results' | 'qc' | 'samples' | 'inputs' | 'cost';

/**
 * The sections for a run in `state`, in order: a finished run leads with its results, a failed one
 * with what went wrong, and a live one with its samples.
 */
export function sectionOrder(state: string): SectionId[] {
  switch (state) {
    case 'succeeded':
      return ['results', 'qc', 'samples', 'inputs', 'cost'];
    case 'failed':
      return ['wrong', 'results', 'samples', 'inputs', 'cost'];
    case 'cancelled':
      return ['results', 'samples', 'inputs', 'cost'];
    default:
      return ['samples', 'results', 'inputs', 'cost'];
  }
}

type ProgressRun = Pick<RunDetail, 'samples' | 'completed' | 'total'>;

/** How far a live run is (0..1): in samples against a samplesheet, else in steps; null unknown. */
export function progressFraction(run: ProgressRun): number | null {
  const s = run.samples;
  if (s && s.declared && s.total > 0) return Math.min(1, s.done / s.total);
  if (run.total != null && run.total > 0) return Math.min(1, (run.completed ?? 0) / run.total);
  return null;
}

/** "Started 2h ago by you" (`me` is the signed-in user's email, when known). */
export function startedLine(
  run: Pick<RunDetail, 'submitted_at' | 'submitter' | 'submitter_label'>,
  me: string | null,
): string {
  const label = run.submitter_label ?? null;
  const who = me && label === me ? 'you' : (label ?? run.submitter?.replace(/^user:/, '') ?? null);
  const when = run.submitted_at != null ? `Started ${fmtRelative(run.submitted_at)}` : 'Started';
  return who ? `${when} by ${who}` : when;
}

/**
 * A cancelled run's banner: `Cancelled by priya@lab.example.org` and when, when the server knows who asked;
 * else `Cancelled`, stopped when.
 */
export function cancelledBanner(run: Pick<RunDetail, 'cancelled_by' | 'completed_at'>): {
  label: string;
  detail: string;
} {
  const by = run.cancelled_by?.trim();
  const when = run.completed_at != null ? fmtDate(run.completed_at) : '';
  if (by) return { label: `Cancelled by ${by}`, detail: when };
  return { label: 'Cancelled', detail: when ? `stopped ${when}` : '' };
}

/** Who can open a shared analysis page: anyone signed in with access to its project. */
export function shareSentence(project: string | null | undefined): string {
  const p = project?.trim();
  return p
    ? `Anyone signed in to biffix with access to project ${p} can open it.`
    : 'Anyone signed in to biffix with access to its project can open it.';
}

/**
 * The run's one cost, as the lists show it (`#lib/runs/cost.js`): the estimate so far while it runs,
 * the total once it ended (marked while jobs are still to be priced); null when none of its jobs is
 * priced yet.
 */
export function costText(
  run: Pick<RunDetail, 'cost'>,
  live: boolean,
): { amount: string; note: string } | null {
  const usd = pricedUsd(run.cost);
  if (usd == null) return null;
  const pending = (run.cost?.pending_jobs ?? 0) > 0;
  const note = live
    ? 'estimate'
    : pending
      ? 'Estimated from AWS prices for the machines this analysis used. Some are not priced yet, so it can still grow.'
      : 'Estimated from AWS prices for the machines this analysis used.';
  return { amount: costLine(usd, { live, pending }).text, note };
}
