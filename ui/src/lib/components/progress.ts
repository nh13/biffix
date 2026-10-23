// What <Progress> shows for a run, as a pure function of the run's counts so the bucket arithmetic
// is testable without mounting the component.

import type { RunSummary } from '$lib/api';
import { isTerminalRun } from '$lib/runs/runState';

/** The run fields progress reads (both `RunSummary` and `RunDetail` carry them). */
export type ProgressFacts = Pick<
  RunSummary,
  | 'terminal'
  | 'completed'
  | 'total'
  | 'tasks_observed'
  | 'tasks_succeeded'
  | 'tasks_running'
  | 'tasks_queued'
  | 'tasks_failed'
  | 'tasks_cached'
>;

export interface ProgressView {
  terminal: boolean;
  /** A plan total exists, so a filled bar has a real denominator. */
  determinate: boolean;
  total: number | null;
  done: number;
  /** Whole percent done, 100 only once `done >= total`. */
  pct: number;
  succeeded: number;
  running: number;
  queued: number;
  failed: number;
  cached: number;
  observed: number;
  /**
   * Observed rows in no other bucket (never below 0, as an engine's counts can overlap). `cached`
   * is a subset of `succeeded`, so the chips with it shown sum to at least `observed`.
   */
  unclassified: number;
  /** The run ended without scheduling a job or reporting a plan: nothing to show. */
  noJobs: boolean;
}

export function progressView(run: ProgressFacts): ProgressView {
  const terminal = isTerminalRun(run);
  const total = run.total ?? null;
  const observed = run.tasks_observed ?? 0;
  const succeeded = run.tasks_succeeded ?? 0;
  // Determinate only when the engine reported a plan. Key on `total`, NOT engine identity.
  const determinate = total != null && total > 0;
  // `completed` is the engine's own count; fall back to observed successes if a plan exists but the
  // completed count was not populated.
  const done = run.completed ?? succeeded;
  // Floor, not round: 199 of 200 is 99%, never a "100%" that claims a completion that hasn't
  // happened. Clamped for an engine count that overshoots its plan.
  const pct = determinate ? Math.min(100, Math.max(0, Math.floor((done / total) * 100))) : 0;
  // The projection resolves a task left open on an ended run (stranded → failed, see the reducer's
  // `effective_task_state`), so a terminal run's running/queued counts are 0 by construction.
  const running = run.tasks_running ?? 0;
  const queued = run.tasks_queued ?? 0;
  const failed = run.tasks_failed ?? 0;
  const unclassified = Math.max(0, observed - succeeded - running - queued - failed);
  return {
    terminal,
    determinate,
    total,
    done,
    pct,
    succeeded,
    running,
    queued,
    failed,
    cached: run.tasks_cached ?? 0,
    observed,
    unclassified,
    noJobs: !determinate && observed === 0 && terminal,
  };
}
