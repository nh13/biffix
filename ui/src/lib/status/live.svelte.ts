// The live `GET /status` probe, shared: the root layout's health badge and the status page both
// read it, so one poll serves both instead of each sending the same request every interval.
//
// Unlike the data views, this reflects EVERY probe, a failure included: after one good probe a
// later failure sets `error` (keeping the last good `report` for context), so neither the badge nor
// the page keeps saying "operational" while the control plane is unreachable.

import { getStatus, type StatusReport } from '#lib/api/index.js';
import { startPoll } from '#lib/util/poll.js';
import { healthOfStatus, type Health } from './health';
import { errorMessage } from '#lib/util/format.js';

export const STATUS_POLL_MS = 15000;

export const liveStatus = $state<{
  /** The last report a probe returned (kept, and shown as stale, while `error` is set). */
  report: StatusReport | null;
  /** Why the latest probe failed; `null` once a probe succeeds. */
  error: string | null;
  /** When `report` was fetched. */
  checkedAt: Date | null;
  /** The first probe has settled (success or failure). */
  settled: boolean;
}>({ report: null, error: null, checkedAt: null, settled: false });

export type { Health };

/**
 * The badge verdict for the shared poll's latest probe: red when unreachable, grey until the first
 * answer, else the report's status mapped by `healthOfStatus` (an unknown status is a warning,
 * never claimed healthy).
 */
export function healthOf(report: StatusReport | null, error: string | null): Health {
  if (error) return healthOfStatus(null);
  if (!report) return 'unknown';
  return healthOfStatus(report.status);
}

type Probe = { ok: true; report: StatusReport } | { ok: false; error: string };

// The fetcher maps a failure to a value and never throws, so every probe reaches onData and the
// loop mechanics stay in the shared startPoll.
async function probe(): Promise<Probe> {
  try {
    return { ok: true, report: await getStatus() };
  } catch (e) {
    return { ok: false, error: errorMessage(e) };
  }
}

// Others that refresh on the same beat (the notification bell polls with the 15 s status poll),
// so one timer serves them all, hidden-tab pauses included.
const probeListeners = new Set<() => void>();

/** Call `listener` after every probe of the shared poll; returns its removal. */
export function onStatusProbe(listener: () => void): () => void {
  probeListeners.add(listener);
  return () => probeListeners.delete(listener);
}

function apply(p: Probe) {
  if (p.ok) {
    liveStatus.report = p.report;
    liveStatus.error = null;
    liveStatus.checkedAt = new Date();
  } else {
    liveStatus.error = p.error;
  }
  liveStatus.settled = true;
  for (const listener of probeListeners) listener();
}

let watchers = 0;
let stop: (() => void) | undefined;

/**
 * Keep the shared probe running while anything watches it. The first watcher starts the poll; the
 * last one to release stops it. Returns the release (return it from an `$effect`).
 */
export function watchStatus(): () => void {
  if (watchers++ === 0) {
    stop = startPoll({ fetch: probe, onData: apply, intervalMs: STATUS_POLL_MS });
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    if (--watchers === 0) {
      stop?.();
      stop = undefined;
    }
  };
}
