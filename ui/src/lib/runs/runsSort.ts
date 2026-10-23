// The runs list's client-side sort of the page it shows (cross-page order stays the server's:
// recent first), and where the list goes after a dismissal. Pure, so each is unit-tested.

import type { RunSummary } from '#lib/api/index.js';
import type { SortDir } from '#lib/util/sort.js';
import { pipelineTitle } from './runName';
import type { PageRequest } from './runsPager';

export type SortKey =
  | 'run'
  | 'name'
  | 'workflow'
  | 'state'
  | 'engine'
  | 'project'
  | 'submitted'
  | 'completed'
  | 'submitter';

/**
 * `runs` sorted by `key` (a stable sort: equal values keep the server's order). `nameOf` is how the
 * view names a run, and `lab` whether it is the Lab view, which sorts the workflow column by the
 * pipeline's title. A time not known yet sorts as the earliest.
 */
export function sortRuns(
  runs: readonly RunSummary[],
  key: SortKey,
  dir: SortDir,
  { nameOf, lab }: { nameOf: (r: RunSummary) => string; lab: boolean },
): RunSummary[] {
  const sign = dir === 'asc' ? 1 : -1;
  const val = (r: RunSummary): string | number => {
    switch (key) {
      case 'run':
        return r.run_id;
      case 'name':
        return nameOf(r).toLowerCase();
      case 'workflow':
        return ((lab ? pipelineTitle(r) : r.workflow_name) ?? r.workflow ?? '').toLowerCase();
      case 'state':
        return r.state;
      case 'engine':
        return r.engine ?? '';
      case 'project':
        return r.project ?? '';
      case 'submitted':
        return r.submitted_at ?? 0;
      case 'completed':
        return r.completed_at ?? 0;
      case 'submitter':
        return r.submitter_label ?? r.submitter ?? '';
    }
  };
  return [...runs].sort((a, b) => {
    const av = val(a);
    const bv = val(b);
    if (av < bv) return -sign;
    if (av > bv) return sign;
    return 0;
  });
}

/**
 * Where the list goes after a dismissal: back to the first page when this page is now empty while
 * runs still need attention elsewhere (`left`: how many, null when unknown) and it is not the first
 * page already; else it refreshes where it is.
 */
export function afterDismissal(
  shown: number,
  left: number | null,
  at: { index: number; req: PageRequest },
): 'first-page' | 'refresh' {
  const emptied = shown === 0 && left !== 0;
  return emptied && (at.index > 0 || !!at.req.after || !!at.req.before) ? 'first-page' : 'refresh';
}
