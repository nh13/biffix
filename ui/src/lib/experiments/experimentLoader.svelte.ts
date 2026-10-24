// One experiment's detail, as both views show it: loaded, then polled while any of its analyses is
// running.

import { getExperiment, type Experiment, type ExperimentDetail } from '#lib/api/index.js';
import { errorKind as toErrorKind, type ErrorKind } from '#lib/components/errorPage.js';
import { PollNudge, startPoll } from '#lib/util/poll.js';
import { anyLive } from './experiments';

export class ExperimentLoader {
  detail = $state<ExperimentDetail | null>(null);
  loading = $state(true);
  error = $state<string | null>(null);
  errorKind = $state<ErrorKind>('server');
  #attempt = $state(0);
  /** Edits saved on this page, and the last one's experiment: a load that left before it is stale. */
  #edits = 0;
  #saved: Experiment | null = null;
  readonly #nudge = new PollNudge();

  /**
   * Load experiment `id` and poll it until none of its analyses is live; returns the stop. Call it
   * from an `$effect`, which reruns it for another id or a retry.
   */
  watch(id: string): () => void {
    void this.#attempt;
    this.detail = null;
    this.loading = true;
    this.error = null;
    this.#saved = null;
    return startPoll({
      fetch: async () => {
        const edits = this.#edits;
        return { d: await getExperiment(id), stale: () => edits !== this.#edits };
      },
      onData: ({ d, stale }) => {
        // Read before an edit was saved: keep the edit (the nudge brings a fresh read).
        this.detail = stale() && this.#saved ? { ...d, experiment: this.#saved } : d;
        this.error = null;
      },
      onError: (m, e) => {
        this.error = m;
        this.errorKind = toErrorKind(e);
      },
      onSettled: () => (this.loading = false),
      until: ({ d }) => !anyLive(d.runs),
      intervalMs: 15_000,
      nudge: this.#nudge,
    });
  }

  /** Load it afresh (after a failed first load). */
  retry(): void {
    this.#attempt++;
  }

  /** An edit was saved: show the experiment as it is now, over any load already on its way. */
  updated(experiment: Experiment): void {
    this.#edits++;
    this.#saved = experiment;
    if (this.detail) this.detail = { ...this.detail, experiment };
    this.#nudge.now();
  }
}
