// Why a failed run failed, as the Expert view's *Why this run failed* and the Lab view's *What went
// wrong* both read it: asked once per run that failed, and again while the server says the summary
// is still `pending` (the engine reported the failure before its last output was in). Each view
// words it its own way.
//
// A component owns one loader and starts it from an `$effect` keyed on primitives read off the run
// (a live run's page replaces `run` on every poll, so keying on the object would ask on every poll):
//
//   const why = new FailureSummaryLoader();
//   $effect(() => why.watch(runId, failed));

import { getFailureSummary, type FailureSummary } from '#lib/api/index.js';
import { errorMessage } from '#lib/util/format.js';

/** How long after a pending summary it is asked for again. */
export const SETTLE_MS = 3000;
/**
 * The most re-asks of a summary that stays pending: enough to outlast the window the server may say
 * `pending` for (biffix-api's `FAILURE_SUMMARY_SETTLE_SECS`, which the test reads from the API doc).
 */
export const SETTLE_TRIES = 40;

export class FailureSummaryLoader {
  summary = $state<FailureSummary | null>(null);
  /** Why the summary could not be loaded. */
  error = $state<string | null>(null);

  /**
   * Ask for `runId`'s summary if it failed, and again while it is pending; returns the cancel (it
   * stops the re-asks, and a later answer is dropped).
   */
  watch(runId: string, failed: boolean): (() => void) | undefined {
    this.summary = null;
    this.error = null;
    if (!failed) return;
    // Not `startPoll`: it ticks on an interval, keeps polling after a failed first load or a
    // failed refresh, and counts only answers, where this asks again only after a pending answer,
    // and stops at the first failure.
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ask = (tries: number) => {
      getFailureSummary(runId)
        .then((s) => {
          if (cancelled) return;
          this.summary = s;
          if (s.pending && tries < SETTLE_TRIES)
            timer = setTimeout(() => ask(tries + 1), SETTLE_MS);
        })
        .catch((e) => {
          // A re-ask that fails keeps the summary already shown, and stops.
          if (!cancelled && tries === 0) this.error = errorMessage(e);
        });
    };
    ask(0);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }
}
