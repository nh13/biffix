// A version's queue assignment (`queues:` in its biffix.yaml): which worker queue-class each task
// runs on, as the Provenance tab lists it.

import type { components } from '#lib/generated/api.js';

export type VersionQueues = components['schemas']['VersionQueues'];

/** One line of the assignment: the tasks it picks, and the queue-class they run on. */
export interface QueueRow {
  /** `Default`, `Label <label>` (Nextflow) or `Name <name>` (a process or rule). */
  tasks: string;
  queueClass: string;
}

/**
 * The assignment's lines in the order the engines rank them, least specific first: the default,
 * then each label, then each name. Without a `default`, tasks no selector picks run on the
 * version's first queue-class (`queueClasses`), so the default line shows that one. Empty for no
 * assignment.
 */
export function queueRows(
  queues: VersionQueues | null | undefined,
  queueClasses: readonly string[] = [],
): QueueRow[] {
  if (!queues) return [];
  const sorted = (selectors: Record<string, string> | undefined) =>
    Object.entries(selectors ?? {}).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const fallback = queueClasses[0];
  const defaultRow = queues.default
    ? [{ tasks: 'Default', queueClass: queues.default }]
    : fallback
      ? [{ tasks: 'Default (first queue-class)', queueClass: fallback }]
      : [];
  return [
    ...defaultRow,
    ...sorted(queues.by_label).map(([label, queueClass]) => ({
      tasks: `Label ${label}`,
      queueClass,
    })),
    ...sorted(queues.by_name).map(([name, queueClass]) => ({ tasks: `Name ${name}`, queueClass })),
  ];
}
