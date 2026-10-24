// A running analysis's progress in the Lab view when it has no samplesheet to count
// against: a bar from its steps (tasks) done against the known total,
// a strip with each step's state in pipeline order, and how long it has run against how long the
// pipeline usually takes. Only facts: no total is invented (a Nextflow run's total grows as it
// finds work, so its bar never reaches the end while it runs), and no time left is shown without
// the pipeline's past runs to go by. Pure, so it is unit-tested.

import type { RunDetail, WorkflowEstimate } from '#lib/api/index.js';
import { layout } from '#lib/runs/dag/layout.js';
import { plural } from '#lib/util/format.js';
import { fmtHours } from '#lib/launch/estimate.js';

type ProgressRun = Pick<
  RunDetail,
  'completed' | 'total' | 'tasks_observed' | 'tasks_succeeded' | 'tasks_cached'
>;

export interface StepProgress {
  /** How far along (0..1) the bar is drawn. */
  fraction: number;
  /** The total is not final: the pipeline is still finding steps to run. */
  growing: boolean;
  /** `7 of 12 steps done`, or `2 of 5+ steps done` while the total grows. */
  text: string;
}

const steps = (n: number) => plural(n, 'step');

/**
 * The run's progress in steps: against the engine's plan when it reported one (Snakemake knows its
 * whole DAG up front), else against the steps seen so far, plus one, so a run still finding work
 * never shows a full bar. Null before any step is known.
 */
export function stepProgress(run: ProgressRun): StepProgress | null {
  const total = run.total ?? null;
  if (total != null && total > 0) {
    const done = Math.min(total, Math.max(0, run.completed ?? run.tasks_succeeded ?? 0));
    return { fraction: done / total, growing: false, text: `${done} of ${steps(total)} done` };
  }
  const seen = run.tasks_observed ?? 0;
  if (seen <= 0) return null;
  const done = Math.min(seen, run.tasks_succeeded ?? 0);
  return {
    fraction: done / (seen + 1),
    growing: true,
    text: `${done} of ${seen}+ steps done`,
  };
}

export type StepState = 'done' | 'running' | 'waiting' | 'failed' | 'none';

export interface StepChip {
  /** The engine's name for the step (rule or process): the chip's key. */
  group: string;
  state: StepState;
  /** Its tasks finished, and its tasks known so far. */
  done: number;
  total: number;
}

type Dag = RunDetail['dag'];

/**
 * The run's steps in pipeline order, the order the DAG tab draws them in (`dag/layout`'s layers top
 * to bottom, each layer left to right), so the strip and the graph never disagree; each with its
 * state: failed if a task failed, running if one runs, done when all known tasks finished, waiting
 * when some are known but none started, else not started. Empty when the engine reported no graph.
 */
export function stepChips(dag: Dag | null | undefined): StepChip[] {
  const nodes = dag?.nodes ?? [];
  const order = layout(
    nodes.map((n) => n.id),
    (dag?.edges ?? []) as [string, string][],
  ).nodes.map((n) => n.id);
  const byId = new Map(nodes.map((n) => [n.id, n]));
  return order.map((id) => {
    const n = byId.get(id)!;
    const total = n.pending + n.running + n.succeeded + n.failed;
    const stepState: StepState =
      n.failed > 0
        ? 'failed'
        : n.running > 0
          ? 'running'
          : total > 0 && n.succeeded === total
            ? 'done'
            : n.pending > 0
              ? 'waiting'
              : 'none';
    return { group: id, state: stepState, done: n.succeeded, total };
  });
}

/** A span of seconds in words: `under a minute`, `40 min`, `1 h 10 min`, `26 h`. Not
 * `fmtWallTime`, which says a finished run's time (`1h 10m`): this is a running one's, rounded for
 * reading at a glance. */
export function fmtSpan(secs: number): string {
  const s = Math.max(0, secs);
  if (s < 60) return 'under a minute';
  const mins = Math.round(s / 60);
  if (mins < 60) return `${mins} min`;
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  if (h >= 10) return `${Math.round(s / 3600)} h`;
  return m === 0 ? `${h} h` : `${h} h ${m} min`;
}

type Typical = NonNullable<WorkflowEstimate['typical']>;

export interface TimeFacts {
  /** `Running for 40 min` */
  elapsed: string;
  /**
   * Against the pipeline's usual run time, when it has one: `usually ~2.5 h · about 1 h 50 min
   * left`, or `usually ~2.5 h · taking longer than usual`. Null without past runs to go by.
   */
  usual: string | null;
}

/**
 * How long a run has run (from `startedAt` to `nowSecs`, epoch seconds) and, from its pipeline's
 * typical run, about how long it has left. The time left is the usual time minus the time so far;
 * past the usual time it says so rather than counting down below zero.
 */
export function timeFacts(
  startedAt: number | null | undefined,
  nowSecs: number,
  typical: Typical | null | undefined,
): TimeFacts | null {
  if (startedAt == null) return null;
  const ran = Math.max(0, nowSecs - startedAt);
  const elapsed = `Running for ${fmtSpan(ran)}`;
  if (!typical || !(typical.hours_median > 0)) return { elapsed, usual: null };
  const usualSecs = typical.hours_median * 3600;
  const usually = `usually ~${fmtHours(typical.hours_median)}`;
  // Within the last minute of the usual time, or past it: no countdown to "under a minute".
  const left = usualSecs - ran;
  return {
    elapsed,
    usual:
      left >= 60
        ? `${usually} · about ${fmtSpan(left)} left`
        : `${usually} · taking longer than usual`,
  };
}

/**
 * A live run's time line, counted from its launch as a typical run is (a wait for submit-time
 * images or in the queue is not run time): before launch it says the analysis is waiting to start
 * (getting its software ready, or queued) and gives no countdown; once launched, [`timeFacts`].
 */
export function liveTimeFacts(
  run: Pick<RunDetail, 'state' | 'launched_at_micros'>,
  nowSecs: number,
  typical: Typical | null | undefined,
): TimeFacts | null {
  if (run.launched_at_micros == null) {
    const elapsed =
      run.state === 'preparing_images'
        ? 'Waiting to start: getting its software ready'
        : 'Waiting to start: queued for a machine';
    const usual =
      typical && typical.hours_median > 0
        ? `usually takes ~${fmtHours(typical.hours_median)} once started`
        : null;
    return { elapsed, usual };
  }
  const facts = timeFacts(run.launched_at_micros / 1e6, nowSecs, typical);
  // Launched but not yet on a machine: its clock runs (a typical run's does too), in words that do
  // not claim it is running.
  if (facts && run.state === 'submitted')
    facts.elapsed = facts.elapsed.replace(/^Running for /, 'Waiting for a machine for ');
  return facts;
}
