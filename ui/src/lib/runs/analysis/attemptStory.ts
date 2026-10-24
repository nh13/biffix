// Each try of a failed step as one plain line for the Lab view's *Why?*:
// when it started, the kind of machine it ran on (in lab words, not instance types), how long it
// ran and why it ended, and whether it was tried again by itself. From the step's attempt
// records only, never its log: logs belong to the Expert view, so the Lab view never shows one.
// Pure, so unit-tested.

import type { TaskAttempt } from '#lib/api/index.js';
import { fmtBytes, fmtDate } from '#lib/util/format.js';
import { attemptEnding } from '#lib/runs/attempts.js';
import { fmtSpan } from './liveProgress';

export type TryTone = 'ok' | 'bad' | 'live';

export interface TryLine {
  /** `Try 1` */
  label: string;
  /** `started Oct 3, 9:12 AM`, when known. */
  started: string | null;
  /** `on a low-cost (spot) machine`, when known. */
  where: string | null;
  /** `with 8.0 GiB of memory`: what it asked for, when known. */
  memory: string | null;
  /** Why it ended (or what it is doing): `ran out of memory after 1 h 10 min`. */
  outcome: string;
  /** It failed and a later try followed: `tried again automatically`. */
  retried: boolean;
  tone: TryTone;
}

/** The kind of machine, in a biologist's words; null when not known. */
export function machineWords(capacity: string | null | undefined): string | null {
  switch (capacity) {
    case 'spot':
      return 'on a low-cost (spot) machine';
    case 'on_demand':
      return 'on a standard (on-demand) machine';
    case null:
    case undefined:
    case '':
      return null;
    default:
      return `on a ${capacity.replaceAll('_', '-')} machine`;
  }
}

/** Why an ended try ended, in plain words (without how long it ran). */
function endedWords(a: TaskAttempt): { text: string; tone: TryTone } {
  if (a.state === 'succeeded') return { text: 'finished', tone: 'ok' };
  // Ranked as the Expert view and the failure diagnosis rank it.
  switch (attemptEnding(a)) {
    case 'spot':
      return { text: 'AWS took the machine back', tone: 'bad' };
    case 'oom':
      return { text: 'ran out of memory', tone: 'bad' };
    case 'host_terminated':
      return { text: 'the machine stopped unexpectedly', tone: 'bad' };
    case 'missing_output':
      return { text: 'finished without writing the files it should have', tone: 'bad' };
  }
  if (a.exit_code === 137)
    return { text: 'was stopped (often a sign of running out of memory)', tone: 'bad' };
  if (a.exit_code != null && a.exit_code !== 0)
    return { text: `stopped with an error (exit code ${a.exit_code})`, tone: 'bad' };
  return { text: 'stopped with an error', tone: 'bad' };
}

/** What a try that has not ended is doing. */
function liveWords(a: TaskAttempt): string {
  return a.state === 'running' ? 'running now' : 'waiting for a machine';
}

/** What a try still open when its analysis ended says (`terminationLabel`'s `run_ended`). */
const NO_RESULT = 'no result reported (the analysis ended)';

/**
 * The step's tries, oldest first, one line each. `attempts` come in the server's order (by attempt
 * number); a try without a number is numbered by its place. `runEnded`: the analysis has ended (as
 * every analysis *Why?* explains has), so a try still running or queued in the records was left
 * without a result, never "running now".
 */
export function attemptStory(
  attempts: readonly TaskAttempt[],
  { runEnded = false }: { runEnded?: boolean } = {},
): TryLine[] {
  return attempts.map((a, i) => {
    const ended = a.state === 'succeeded' || a.state === 'failed';
    const ran =
      a.started_at != null && a.finished_at != null && a.finished_at >= a.started_at
        ? ` after ${fmtSpan(a.finished_at - a.started_at)}`
        : '';
    const end = ended
      ? endedWords(a)
      : runEnded
        ? { text: NO_RESULT, tone: 'bad' as TryTone }
        : null;
    const memory =
      a.requested_memory_mib != null
        ? `with ${fmtBytes(a.requested_memory_mib * 1024 * 1024)} of memory`
        : null;
    return {
      label: `Try ${a.attempt ?? i + 1}`,
      started: a.started_at != null ? `started ${fmtDate(a.started_at)}` : null,
      where: machineWords(a.host?.capacity),
      memory,
      outcome: end ? `${end.text}${ran}` : liveWords(a),
      retried: a.state === 'failed' && i < attempts.length - 1,
      tone: end ? end.tone : 'live',
    };
  });
}

/** One try as a sentence:
 * `Try 1 · started … · on a low-cost (spot) machine · … · tried again automatically`. */
export function tryText(t: TryLine): string {
  return [
    t.label,
    t.started,
    t.where,
    t.memory,
    t.outcome,
    t.retried ? 'tried again automatically' : null,
  ]
    .filter(Boolean)
    .join(' · ');
}
