// One place for the "load once, then poll on an interval until the component goes away" loop that
// every live view needs, so its rules hold everywhere: only the FIRST load surfaces an error or
// clears the spinner; a background refresh keeps the last good view on a transient failure rather
// than clobbering it.
// While the tab is hidden the ticks are skipped (nobody is looking, and every open tab would
// otherwise keep hitting the API); when it is shown again the view refreshes at once.
//
// `startPoll` is a plain function (no runes), so it unit-tests with fake timers and stays framework-
// agnostic. A component keeps its own `$state` and wires the loop from an `$effect`, returning the
// teardown this hands back:
//
//   $effect(() => {
//     filter; query;                       // read reactive deps so the effect re-runs on change
//     return startPoll({
//       fetch: () => listRuns(filter, { q: query }),
//       onData: (r) => { runs = r.runs; error = null; },
//       onError: (m) => (error = m),
//       onSettled: () => (loading = false),
//       intervalMs: 5000,
//     });
//   });

import { errorMessage } from '$lib/util/format';

export interface PollHandlers<T> {
  /** Fetch the latest value. Re-invoked on each tick until stopped. */
  fetch: () => Promise<T>;
  /** Apply a successful result. Called for the initial load AND every background refresh. */
  onData: (value: T) => void;
  /** A failure of the INITIAL load only (background failures keep the last good view). */
  onError?: (message: string, error: unknown) => void;
  /** A failure of a background refresh: the last good view is kept, so this only lets the view say
   * it may be out of date. The next successful refresh calls `onData` as usual. */
  onRefreshError?: (message: string) => void;
  /** Runs once, after the initial load settles (success or failure) — e.g. clear a spinner. */
  onSettled?: () => void;
  /** Stop polling once a result satisfies this (e.g. a run reached a terminal state). */
  until?: (value: T) => boolean;
  /** Poll period in milliseconds. */
  intervalMs: number;
  /** Lets the view ask for a refresh now (after a change it made), not at the next tick. */
  nudge?: PollNudge;
}

/**
 * A view's handle to refresh its poll at once (e.g. right after it dismissed rows, so the next
 * ones are listed now rather than at the next tick). A nudge while a load is pending loads again
 * once it lands, so what is shown is read after the change. Does nothing while no poll uses it.
 */
export class PollNudge {
  #listener: (() => void) | null = null;

  /** Refresh now. */
  now(): void {
    this.#listener?.();
  }

  /** Point the nudge at a poll (`startPoll` does); returns the detach. */
  attach(listener: () => void): () => void {
    this.#listener = listener;
    return () => {
      if (this.#listener === listener) this.#listener = null;
    };
  }
}

/**
 * Start an immediate load followed by interval polling. Returns a teardown that cancels any
 * in-flight handling and clears the interval — return it directly from a Svelte `$effect` so a
 * dependency change or unmount tears the loop down and a fresh one replaces it.
 */
export function startPoll<T>(handlers: PollHandlers<T>): () => void {
  const { fetch, onData, onError, onRefreshError, onSettled, until, intervalMs, nudge } = handlers;
  let cancelled = false;
  // A nudge came while a load was pending: load again once it lands.
  let again = false;
  // A load() is awaiting fetch(). The initial load sets it before the first tick can fire and a
  // tick skips while it is set, so at most one load is ever pending: a slow fetch never piles up
  // concurrent requests, and responses can't land out of order.
  let inFlight = false;
  let timer: ReturnType<typeof setInterval> | undefined;

  // No `document` outside a browser (SSR, unit tests under node): never hidden.
  const doc = typeof document === 'undefined' ? undefined : document;
  const hidden = () => doc?.visibilityState === 'hidden';
  const onVisibility = () => {
    // Shown again: refresh now rather than wait out the interval (unless stopped, or a load is
    // already pending).
    if (!hidden() && timer !== undefined && !inFlight && !cancelled) void load(false);
  };

  const detach = nudge?.attach(() => {
    if (cancelled || timer === undefined) return;
    if (inFlight) again = true;
    else void load(false);
  });

  const stop = () => {
    clearInterval(timer);
    timer = undefined;
    doc?.removeEventListener('visibilitychange', onVisibility);
    detach?.();
  };

  const load = async (initial: boolean) => {
    inFlight = true;
    try {
      const value = await fetch();
      if (cancelled) return;
      onData(value);
      // Stopping clears the interval, so no further tick fires (`timer` is always assigned by
      // now: the fetch above yielded before startPoll returned).
      if (until?.(value)) stop();
    } catch (e) {
      if (!cancelled) {
        const message = errorMessage(e);
        if (initial) onError?.(message, e);
        else onRefreshError?.(message);
      }
    } finally {
      inFlight = false;
      if (!cancelled && initial) onSettled?.();
      if (again && !cancelled && timer !== undefined) {
        again = false;
        void load(false);
      }
    }
  };

  void load(true);
  timer = setInterval(() => {
    if (!inFlight && !hidden()) void load(false);
  }, intervalMs);
  doc?.addEventListener('visibilitychange', onVisibility);

  return () => {
    cancelled = true;
    stop();
  };
}
