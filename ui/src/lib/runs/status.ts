// A run's state in plain words, for the Lab view: one tone (drives the colour) and one line of
// text. One function, so the home page, the analyses list and the analysis page agree. Progress is
// counted in samples when the run's tasks name them, else in steps (tasks).

import type { RunSummary } from '#lib/api/index.js';
import { fmtWallTime, plural } from '#lib/util/format.js';

export type StatusTone = 'waiting' | 'running' | 'done' | 'attention' | 'stopped';

export interface StatusLine {
  tone: StatusTone;
  /** The short label: "Running", "Finished", "Needs attention"… */
  label: string;
  /** What is known beyond the label ("7 of 12 steps done"); empty when nothing is. */
  detail: string;
}

type StatusRun = Pick<
  RunSummary,
  | 'state'
  | 'completed'
  | 'total'
  | 'tasks_succeeded'
  | 'tasks_failed'
  | 'submitted_at'
  | 'completed_at'
  | 'samples'
>;

const steps = (n: number) => plural(n, 'step');
const samples = (n: number) => plural(n, 'sample');

/**
 * The run's progress. In samples when it has them: "9 of 12 samples done" against a samplesheet, or
 * "1 done · 3 samples seen so far" while the set is still open. Else in steps: "7 of 12 steps done",
 * or "7 steps done" with no known total.
 */
function progress(run: StatusRun): string {
  const s = run.samples;
  if (s && s.total > 0) {
    const done = s.declared
      ? `${s.done} of ${samples(s.total)} done`
      : `${s.done} done · ${samples(s.total)} seen so far`;
    return s.failed > 0 ? `${done} · ${s.failed} failed` : done;
  }
  // Cache hits are among the succeeded tasks already (`tasks_cached` is a subset).
  const done = run.tasks_succeeded ?? 0;
  if (run.total != null && run.total > 0) {
    return `${Math.min(run.completed ?? done, run.total)} of ${steps(run.total)} done`;
  }
  return done > 0 ? `${steps(done)} done` : '';
}

export function statusLine(run: StatusRun): StatusLine {
  switch (run.state) {
    case 'preparing_images':
      return { tone: 'waiting', label: 'Getting ready', detail: 'preparing the software' };
    case 'submitted':
      return { tone: 'waiting', label: 'Waiting to start', detail: '' };
    case 'running':
      return { tone: 'running', label: 'Running', detail: progress(run) };
    case 'succeeded': {
      const took =
        run.submitted_at != null ? `took ${fmtWallTime(run.submitted_at, run.completed_at)}` : '';
      const n = run.samples?.total ?? 0;
      return {
        tone: 'done',
        label: 'Finished',
        detail: [n > 0 ? samples(n) : '', took].filter(Boolean).join(' · '),
      };
    }
    case 'failed': {
      const s = run.samples;
      if (s && s.failed > 0) {
        const failed = s.declared
          ? `${s.failed} of ${samples(s.total)} failed`
          : `${samples(s.failed)} failed`;
        return {
          tone: 'attention',
          label: 'Needs attention',
          detail: s.done > 0 ? `${failed} · ${s.done} finished` : failed,
        };
      }
      const failed = run.tasks_failed ?? 0;
      return {
        tone: 'attention',
        label: 'Needs attention',
        detail: failed > 0 ? `${steps(failed)} failed` : 'stopped before its steps ran',
      };
    }
    case 'cancelled':
      return { tone: 'stopped', label: 'Cancelled', detail: progress(run) };
    default:
      return { tone: 'waiting', label: run.state.replaceAll('_', ' '), detail: '' };
  }
}
