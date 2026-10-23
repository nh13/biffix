// The Jobs view's state in the URL fragment (`/runs/<id>/jobs#state=…&group=…&q=…&sort=…&order=desc&job=…`),
// so a filtered view or a selected job can be shared and survives back/forward. The fragment, not
// the query: the search text and the selected job (a task key, whose wildcards name a sample) never
// reach a server. Defaults are left out, so an unfiltered view has a clean URL.

import type { TaskSort, TaskStateFilter } from '$lib/api';
import { runTabHref } from '$lib/runs/runNav';
import { validParam, withViewParams } from '$lib/util/urlParam';

export const STATE_FILTERS: readonly TaskStateFilter[] = ['all', 'active', 'succeeded', 'failed'];
export const TASK_SORTS: readonly TaskSort[] = ['key', 'state', 'duration', 'started'];

/** Every URL key the Jobs view owns (all of them live in the fragment). */
export const JOBS_VIEW_KEYS = ['state', 'group', 'q', 'sort', 'order', 'job'] as const;

export interface JobsView {
  state: TaskStateFilter;
  group: string | null;
  q: string;
  sort: TaskSort;
  descending: boolean;
  /** The selected job, whose detail is open. */
  job: string | null;
}

export const DEFAULT_JOBS_VIEW: JobsView = {
  state: 'all',
  group: null,
  q: '',
  sort: 'key',
  descending: false,
  job: null,
};

/** The view `params` ask for; unknown values fall back to the defaults. */
export function parseJobsView(params: URLSearchParams): JobsView {
  return {
    state: validParam(params.get('state'), STATE_FILTERS, DEFAULT_JOBS_VIEW.state),
    group: params.get('group') || DEFAULT_JOBS_VIEW.group,
    q: params.get('q') ?? DEFAULT_JOBS_VIEW.q,
    sort: validParam(params.get('sort'), TASK_SORTS, DEFAULT_JOBS_VIEW.sort),
    descending: params.get('order') === 'desc',
    job: params.get('job') || DEFAULT_JOBS_VIEW.job,
  };
}

/** `view` as params, defaults left out, onto a copy of `base` (other params are kept). */
export function jobsViewParams(view: JobsView, base?: URLSearchParams): URLSearchParams {
  const params = new URLSearchParams(base);
  const set = (key: (typeof JOBS_VIEW_KEYS)[number], value: string | null) => {
    if (value) params.set(key, value);
    else params.delete(key);
  };
  set('state', view.state === DEFAULT_JOBS_VIEW.state ? null : view.state);
  set('group', view.group);
  set('q', view.q.trim() || null);
  set('sort', view.sort === DEFAULT_JOBS_VIEW.sort ? null : view.sort);
  set('order', view.descending ? 'desc' : null);
  set('job', view.job);
  return params;
}

/**
 * `url` (the Jobs view's own) showing `view`: the view in the fragment, the fragment's other
 * params and the query kept, but none of the view's keys left in the query. `viewParams` lets a
 * query fill in what the fragment lacks, so a view key left there (a hand-written `?state=failed`)
 * could never be cleared back to its default.
 */
export function jobsViewHref(url: URL, view: JobsView): string {
  const query = new URLSearchParams(url.search);
  for (const key of JOBS_VIEW_KEYS) query.delete(key);
  const search = query.size > 0 ? `?${query}` : '';
  return withViewParams(
    `${url.pathname}${search}`,
    jobsViewParams(view, new URLSearchParams(url.hash.slice(1))),
  );
}

/**
 * A link to run `runId`'s Jobs view showing `patch` over the defaults (a state, a step, a search, a
 * selected job), its view in the fragment. Every link into the Jobs view is built here, so the
 * keys have one owner.
 */
export function jobsHref(runId: string, patch: Partial<JobsView> = {}): string {
  return withViewParams(
    runTabHref(runId, 'jobs'),
    jobsViewParams({ ...DEFAULT_JOBS_VIEW, ...patch }),
  );
}
