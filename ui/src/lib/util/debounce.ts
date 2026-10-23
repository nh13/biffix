/** A debounced function: each call restarts the wait, and `cancel()` drops a call still waiting. */
export interface Debounced<A extends unknown[]> {
  (...args: A): void;
  cancel(): void;
}

/** Call `fn` once calls have paused for `ms`, with the arguments of the last call. */
export function debounce<A extends unknown[]>(fn: (...args: A) => void, ms: number): Debounced<A> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const debounced = (...args: A) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
  debounced.cancel = () => clearTimeout(timer);
  return debounced;
}

/**
 * Call `fn` after `ms`; returns a function that cancels it. Shaped for an `$effect`, which calls
 * that teardown when it re-runs, so a value that keeps changing is acted on only once it settles:
 *
 *   $effect(() => {
 *     const s = search;
 *     return delayed(() => (debounced = s), 300);
 *   });
 */
export function delayed(fn: () => void, ms: number): () => void {
  const timer = setTimeout(fn, ms);
  return () => clearTimeout(timer);
}
