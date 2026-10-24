// Why a failed run failed, as the Expert view's *Why this run failed* and the Lab view's *What went
// wrong* both read it: asked once per run that failed. Each view words it its own way.
//
// A component owns one loader and starts it from an `$effect` keyed on primitives read off the run
// (a live run's page replaces `run` on every poll, so keying on the object would ask on every poll):
//
//   const why = new FailureSummaryLoader();
//   $effect(() => why.watch(runId, failed));

import { getFailureSummary, type FailureSummary } from '#lib/api/index.js';
import { errorMessage } from '#lib/util/format.js';

export class FailureSummaryLoader {
  summary = $state<FailureSummary | null>(null);
  /** Why the summary could not be loaded. */
  error = $state<string | null>(null);

  /** Ask for `runId`'s summary if it failed; returns the cancel (a later answer is dropped). */
  watch(runId: string, failed: boolean): (() => void) | undefined {
    this.summary = null;
    this.error = null;
    if (!failed) return;
    let cancelled = false;
    getFailureSummary(runId)
      .then((s) => {
        if (!cancelled) this.summary = s;
      })
      .catch((e) => {
        if (!cancelled) this.error = errorMessage(e);
      });
    return () => {
      cancelled = true;
    };
  }
}
