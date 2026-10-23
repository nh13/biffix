// Dismissing analyses from *Needs attention*: one row's Dismiss, or
// *Dismiss these N* / *Dismiss all N* naming exactly the rows on screen. Opening a run never clears
// it (that only reads its bell notification); a dismissal, or a Try again, does.
//
// Optimistic: a dismissed run is hidden at once and stays hidden for the life of the page (the
// server never lists a dismissed run again, so a list read while the dismissal was on its way
// cannot bring it back). If the server refuses, the runs are shown again and the error kept for the
// page to say. Once the server has it, the caller hears how many still need attention, so it can
// list the next ones at once rather than wait for its poll. Each surface that lists Needs attention
// keeps its own `Dismissals`.

import { dismissAttention } from '$lib/api';
import { errorMessage } from '$lib/util/format';

/** A list of analyses as the server counted it. */
export interface CountedRuns<T extends { run_id: string }> {
  runs: T[];
  total: number;
}

/**
 * `row` without the runs in `hidden`, its total lowered by as many as were taken out (the server
 * counted them; once it has the dismissal it no longer lists them, and nothing is taken twice).
 */
export function withoutDismissed<T extends { run_id: string }>(
  row: CountedRuns<T>,
  hidden: ReadonlySet<string>,
): CountedRuns<T> {
  if (!hidden.size) return row;
  const runs = row.runs.filter((r) => !hidden.has(r.run_id));
  return { runs, total: Math.max(0, row.total - (row.runs.length - runs.length)) };
}

/**
 * The dismiss button's label for the `shown` rows of `total`: `Dismiss all 3` when the rows are
 * every one, else `Dismiss these 5` (it dismisses exactly the rows it names, never one off screen).
 */
export function dismissAllLabel(shown: number, total: number): string {
  return shown >= total ? `Dismiss all ${shown}` : `Dismiss these ${shown}`;
}

export class Dismissals {
  /** Runs dismissed on this page (waiting on the server, or taken). */
  hidden = $state<ReadonlySet<string>>(new Set());
  /** Why the latest dismissal was refused; `null` once one is taken. */
  error = $state<string | null>(null);
  /** What the latest dismissal did, for a screen reader. */
  announcement = $state('');
  /** The dismissal each run is waiting on, while the server has not answered. */
  #pending = new Map<string, Promise<boolean>>();

  /**
   * Dismiss `runIds` (described as `what` for the announcement): hidden at once, then told to the
   * server; shown again if it refuses. `onTaken` hears how many still need attention once the
   * server has it (`null` when it could not count them). Resolves to whether every one of `runIds`
   * is dismissed: a run already dismissed counts only once its own dismissal is confirmed.
   */
  async dismiss(
    runIds: string[],
    what: string,
    onTaken?: (attention: number | null) => void,
  ): Promise<boolean> {
    // Runs still waiting on an earlier dismissal: this one is only as good as that one.
    const waits = [...new Set(runIds.flatMap((id) => this.#pending.get(id) ?? []))];
    const fresh = runIds.filter((id) => !this.hidden.has(id));
    if (fresh.length) {
      this.hidden = new Set([...this.hidden, ...fresh]);
      this.error = null;
      this.announcement = `Dismissed ${what}.`;
      const sent = this.#send(fresh, what, onTaken);
      for (const id of fresh) this.#pending.set(id, sent);
      waits.push(sent);
    }
    return (await Promise.all(waits)).every(Boolean);
  }

  async #send(
    fresh: string[],
    what: string,
    onTaken?: (attention: number | null) => void,
  ): Promise<boolean> {
    try {
      const { attention } = await dismissAttention(fresh);
      onTaken?.(attention ?? null);
      return true;
    } catch (e) {
      const back = new Set(this.hidden);
      for (const id of fresh) back.delete(id);
      this.hidden = back;
      this.error = errorMessage(e);
      this.announcement = `Couldn't dismiss ${what}.`;
      return false;
    } finally {
      for (const id of fresh) this.#pending.delete(id);
    }
  }
}
