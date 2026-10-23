// The Jobs view's table, its pure parts: what a cell without a Batch job says, and j/k moving the
// selection. Pure, so each is unit-tested.

/**
 * What a screen reader hears for a Batch-only cell (Exit, Requested, Host) on a task that never had
 * a Batch job (a cache hit or a local rule): not applicable, as opposed to not yet known.
 */
export const NO_BATCH_JOB = 'not applicable: no Batch job';

/**
 * The row j (`step` 1) or k (-1) selects among `n` rows from the selected row `at` (-1 when none
 * is: j takes the first, k the last), stopping at either end.
 */
export function nextJobIndex(at: number, step: 1 | -1, n: number): number {
  if (at < 0) return step > 0 ? 0 : n - 1;
  return Math.min(n - 1, Math.max(0, at + step));
}
