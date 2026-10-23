// The run page's tabs: their order, labels, hrefs, counts, and which one a URL is on. Pure, so the
// tab strip stays declarative and this is unit-tested.

import type { RunDetail } from '$lib/api';
import { pricedUsd } from '$lib/runs/cost';
import { paramEntries } from '$lib/runs/runFacts';

export type RunTabId =
  'overview' | 'graph' | 'jobs' | 'outputs' | 'reports' | 'coordinator' | 'cost' | 'params';

export interface RunTab {
  id: RunTabId;
  label: string;
  href: string;
  /** A count shown after the label; null ⇒ none. */
  count: number | null;
  /** Nothing to show (e.g. no cost estimate yet); the tab is dimmed but still reachable. */
  empty: boolean;
}

/** Each tab's path segment under `/runs/<id>`; Overview is the run's own URL. */
const SEGMENT: Record<RunTabId, string> = {
  overview: '',
  graph: 'graph',
  jobs: 'jobs',
  outputs: 'outputs',
  reports: 'reports',
  coordinator: 'coordinator',
  cost: 'cost',
  params: 'params',
};

/** The tabs' own path segments (every tab but Overview), for whatever classifies run URLs. */
export const RUN_TAB_SEGMENTS: readonly string[] = Object.values(SEGMENT).filter((s) => s !== '');

/** The href of `tab` for run `runId`. */
export function runTabHref(runId: string, tab: RunTabId): string {
  const base = `/runs/${encodeURIComponent(runId)}`;
  return SEGMENT[tab] ? `${base}/${SEGMENT[tab]}` : base;
}

/**
 * How many outputs `run` has, as its outputs list counts them: for a resumed run, the list's
 * `listTotal` once known (it includes the files the run reused from the runs it resumed, which its
 * projection's `outputs_total` leaves out); otherwise `outputs_total`.
 */
export function runOutputsTotal(run: RunDetail, listTotal: number | null = null): number {
  return run.resumed_from && listTotal != null ? listTotal : (run.outputs_total ?? 0);
}

/**
 * Whether `run` may have outputs to show: it registered some, or it resumed a run (a resume lists
 * the files it reused, though it may have registered none itself).
 */
export const runMayHaveOutputs = (run: RunDetail): boolean =>
  (run.outputs_total ?? 0) > 0 || !!run.resumed_from;

/**
 * The tabs for `run`, in display order. `outputs` is a resumed run's outputs list total
 * ([`runOutputsTotal`]); null when unknown or the run resumed nothing.
 */
export function runTabs(
  run: RunDetail,
  reports: number | null = null,
  outputs: number | null = null,
): RunTab[] {
  const id = run.run_id;
  const params = paramEntries(run.params).length;
  const tab = (tabId: RunTabId, label: string, count: number | null, empty = false): RunTab => ({
    id: tabId,
    label,
    href: runTabHref(id, tabId),
    count,
    empty,
  });
  return [
    tab('overview', 'Overview', null),
    tab('graph', 'Graph', null, !(run.dag && run.dag.nodes.length > 0)),
    tab('jobs', 'Jobs', run.tasks_observed ?? 0),
    tab('outputs', 'Outputs', runOutputsTotal(run, outputs)),
    tab('reports', 'Reports', reports, reports === 0),
    tab('coordinator', 'Coordinator', null, !run.coordinator_job_id),
    tab('cost', 'Cost', null, pricedUsd(run.cost) === null),
    tab('params', 'Parameters', params, params === 0),
  ];
}

/** Which tab `pathname` is on, for a run page; Overview for the run's own URL or anything else. */
export function activeRunTab(pathname: string): RunTabId {
  const match = /^\/runs\/[^/]+\/([^/]+)\/?$/.exec(pathname);
  const segment = match?.[1];
  const found = (Object.entries(SEGMENT) as [RunTabId, string][]).find(
    ([, s]) => s && s === segment,
  );
  return found ? found[0] : 'overview';
}
