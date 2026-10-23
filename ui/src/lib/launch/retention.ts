// What happens to a Nextflow run's intermediate files (its work dir) once it ends.
// Results are never deleted; a run whose intermediate files are gone can't be resumed.

import { plural } from '$lib/util/format';

export type Retention = 'default' | 'delete_on_success' | 'keep';

export interface RetentionOption {
  value: Retention;
  label: string;
}

/**
 * The launch form's choices, given the deploy's retention period in days (`undefined` ⇒ kept
 * forever, so *Keep* is already the default and isn't offered twice) and the form's `current`
 * value. A `current` of *Keep* (from a re-run or a restored draft) is always offered, so the choice
 * shows: it differs from the default only once the deploy sets a period, which a kept run ignores.
 *
 * The default deletes a run's intermediate files once its period has passed after the run ends,
 * whatever the outcome; *Delete when the run succeeds* deletes them at once, but only on success
 * (a failed run's are kept for the period, to resume from).
 */
export function retentionOptions(
  days: number | null | undefined,
  current?: Retention,
): RetentionOption[] {
  const byDefault =
    days == null
      ? 'Keep (default)'
      : days === 0
        ? 'Delete when the run ends, even if it fails (default)'
        : `Keep for ${plural(days, 'day')} (default)`;
  const options: RetentionOption[] = [
    { value: 'default', label: byDefault },
    { value: 'delete_on_success', label: 'Delete when the run succeeds' },
  ];
  if (days != null) options.push({ value: 'keep', label: 'Keep' });
  else if (current === 'keep')
    options.push({ value: 'keep', label: 'Keep, even if a retention period is set later' });
  return options;
}

/** A recorded retention value as a form choice (anything unknown is the default). */
export function asRetention(value: unknown): Retention {
  return value === 'delete_on_success' || value === 'keep' ? value : 'default';
}
