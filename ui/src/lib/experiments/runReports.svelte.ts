// The key results of several analyses at once (an experiment's), each asked for once it is shown and
// again when its results could have changed (it produced more, or it ended). Shared by every view
// of results across analyses, so switching view never asks again.

import { listRunReports, type RunReport, type RunSummary } from '#lib/api/index.js';
import { errorMessage } from '#lib/util/format.js';
import { untrack } from 'svelte';

export interface RunReportsEntry {
  /** Its reports, once loaded (kept through a reload). */
  reports: RunReport[] | null;
  /** Why the last load failed; null once one succeeds. */
  error: string | null;
}

type Shown = Pick<RunSummary, 'run_id' | 'outputs_total' | 'terminal'>;

/** When a run's reports are asked for again: it wrote more outputs, or it ended. */
const keyOf = (run: Shown) => `${run.outputs_total ?? 0}:${run.terminal}`;

export class RunReportList {
  entries = $state<Record<string, RunReportsEntry>>({});
  /** The key each run was last asked for at (not state: only loads read it). */
  #keys = new Map<string, string>();

  /** Ask for every run in `runs` whose results may have changed since it was last asked for. */
  sync(runs: readonly Shown[]): void {
    untrack(() => {
      for (const run of runs) {
        const key = keyOf(run);
        if (this.#keys.get(run.run_id) === key) continue;
        this.#keys.set(run.run_id, key);
        this.#load(run.run_id, key);
      }
    });
  }

  /** Ask again for one run whose load failed. */
  retry(runId: string): void {
    const key = this.#keys.get(runId);
    if (key != null) untrack(() => this.#load(runId, key));
  }

  #load(runId: string, key: string): void {
    const reports = this.entries[runId]?.reports ?? null;
    this.entries[runId] = { reports, error: null };
    listRunReports(runId)
      .then((r) => {
        if (this.#keys.get(runId) === key)
          this.entries[runId] = { reports: r.reports, error: null };
      })
      .catch((e) => {
        if (this.#keys.get(runId) === key)
          this.entries[runId] = {
            reports: this.entries[runId]?.reports ?? null,
            error: errorMessage(e),
          };
      });
  }
}
