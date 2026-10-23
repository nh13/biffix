// Analyses known only by their run id (a run folder in the Data page, a file one analysis reused
// from another): their summaries, asked in bulk (`POST /runs/query` with `ids`, at most a page of
// them per request), so each is named as everywhere else (`runName`), never by its raw id once it
// is known. An id the server does not list (a folder that is not a run's, a deleted run) stays
// unnamed: its id is shown instead. (Any caller who may list runs lists every run of the tenant, so
// an unlisted id is not another project's run.) What is known is asked again once it is
// {@link NAME_TTL_MS} old (a rename in another tab, a run not listed before), and a rename here
// updates it at once (`put`).

import { SvelteMap } from 'svelte/reactivity';
import { listRuns, type RunSummary } from '#lib/api/index.js';
import { runName } from './runName';

/** The most ids one request names (the backend's page size, `MAX_RUN_PAGE` in
 * crates/biffix-backend/src/store/runs.rs). */
export const IDS_PER_REQUEST = 200;

/** `ids` without blanks and repeats, in batches of at most `size`. */
export function idBatches(ids: Iterable<string>, size = IDS_PER_REQUEST): string[][] {
  const unique = [...new Set([...ids].map((id) => id.trim()).filter(Boolean))];
  const batches: string[][] = [];
  for (let i = 0; i < unique.length; i += size) batches.push(unique.slice(i, i + size));
  return batches;
}

/** How long an answer (a summary, or "not listed") is trusted before it is asked again. */
export const NAME_TTL_MS = 2 * 60_000;

export class RunNames {
  /** Each asked id's summary: `null` when the server did not list it. */
  #known = new SvelteMap<string, RunSummary | null>();
  /** When each id was asked for (answered or in flight); a failed request forgets its ids, to ask
   * again. */
  #asked = new Map<string, number>();
  /** When each id's summary was last set (an answer or a `put`): an answer to an older request does
   * not overwrite a newer one. */
  #setAt = new Map<string, number>();
  readonly #fetch: (ids: string[]) => Promise<RunSummary[]>;
  readonly #now: () => number;

  constructor(
    fetch: (ids: string[]) => Promise<RunSummary[]> = (ids) =>
      listRuns('all', { ids, limit: ids.length }).then((p) => p.runs),
    now: () => number = () => Date.now(),
  ) {
    this.#fetch = fetch;
    this.#now = now;
  }

  /** Ask for the summaries of those of `ids` (trimmed) not asked for yet, or asked for longer than
   * {@link NAME_TTL_MS} ago. */
  want(ids: Iterable<string>): void {
    const now = this.#now();
    const fresh = [...ids]
      .map((id) => id.trim())
      .filter((id) => {
        if (!id) return false;
        const at = this.#asked.get(id);
        return at === undefined || now - at > NAME_TTL_MS;
      });
    for (const batch of idBatches(fresh)) {
      for (const id of batch) this.#asked.set(id, now);
      this.#fetch(batch)
        .then((runs) => {
          const byId = new Map(runs.map((r) => [r.run_id, r]));
          for (const id of batch) {
            // A rename saved since this request went out wins over its answer.
            if ((this.#setAt.get(id) ?? -Infinity) > now) continue;
            this.#known.set(id, byId.get(id) ?? null);
            this.#setAt.set(id, now);
          }
        })
        .catch(() => {
          for (const id of batch) if (this.#asked.get(id) === now) this.#asked.delete(id);
        });
    }
  }

  /** What a rename (or any fresh read) says the run is now: shown at once wherever it is named. */
  put(summary: RunSummary): void {
    const now = this.#now();
    this.#asked.set(summary.run_id, now);
    this.#setAt.set(summary.run_id, now);
    this.#known.set(summary.run_id, summary);
  }

  /** The run's summary, once known (`null` until then, and when the server does not list it). */
  summary(id: string): RunSummary | null {
    return this.#known.get(id.trim()) ?? null;
  }

  /** The analysis's name (`runName`) once its summary is known, else `null`. */
  name(id: string): string | null {
    const s = this.#known.get(id.trim());
    return s ? runName(s) : null;
  }
}

/** The page's shared names: every component naming runs by id asks through this one. */
export const runNames = new RunNames();
