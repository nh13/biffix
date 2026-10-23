// Which pages exist in which view, and where the Lab ⇄ Expert toggle lands. Pure functions over a
// pathname, so the mapping is unit-tested rather than scattered through components.
//
// A page shown in both views follows one convention. Its route is a thin switch: it owns what both
// views share (the URL, the page's frame, a state class such as `RunsList`) and renders one lib
// component per view (`LabHome` / `ExpertHome`, `AnalysisPage` / `RunOverview`), or one component
// given a presentation when only the words differ (`DataBrowser`). Data both views show is read by
// one shared loader (`OutputsLoader`, `FailureSummaryLoader`, `WorkflowCatalog`), not once per view.

import { RUN_TAB_SEGMENTS } from '$lib/runs/runNav';
import { WORKFLOW_TABS, type WorkflowTab } from '$lib/workflows/workflowNav';

export type ViewMode = 'lab' | 'expert';

/**
 * A run's Expert-only tabs, every tab but Overview (from the tab strip's own list, so a new tab is
 * never taken for a Lab page): in Lab view the run's one page (`/runs/{id}`) replaces them, and its
 * file list (`/runs/{id}/files`, [`LAB_RUN_PAGES`]) replaces the Outputs tab.
 */
const EXPERT_RUN_TABS = RUN_TAB_SEGMENTS;
/** A run's Lab pages besides its own, each standing for an Expert tab (Lab page → Expert tab). */
const LAB_RUN_PAGES: Record<string, string> = { files: 'outputs' };
/** A pipeline's tabs in Lab view: how to start it and what it is. The rest are Expert pages. */
export const LAB_WORKFLOW_TABS: readonly WorkflowTab[] = ['run', 'about'];
const EXPERT_WORKFLOW_TABS: readonly string[] = WORKFLOW_TABS.filter(
  (t) => !LAB_WORKFLOW_TABS.includes(t),
);
/** Whole Expert-only pages (not in the Lab nav). */
const EXPERT_PAGES = ['/usage', '/status', '/api-docs', '/settings/references'];

/** The run id and page when `path` is one of a run's own pages (`/runs/{id}/{page}`). */
function runPage(path: string): { run: string; page: string } | null {
  const m = path.match(/^\/runs\/([^/]+)\/([^/]+)\/?$/);
  if (!m || m[1] === 'new') return null;
  return { run: m[1], page: m[2] };
}

/** The run id and tab when `path` is one of a run's Expert-only tabs. */
function expertRunTab(path: string): { run: string; tab: string } | null {
  const p = runPage(path);
  if (!p || !EXPERT_RUN_TABS.includes(p.page)) return null;
  return { run: p.run, tab: p.page };
}

/** The Expert tab a run's Lab page (`/runs/{id}/files`) stands for, else null. */
function expertOfLabRunPage(path: string): string | null {
  const p = runPage(path);
  if (!p || !Object.hasOwn(LAB_RUN_PAGES, p.page)) return null;
  return `/runs/${p.run}/${LAB_RUN_PAGES[p.page]}`;
}

/** The version id when `path` is one of a pipeline's Expert-only tabs. */
function expertWorkflowTab(path: string): string | null {
  const m = path.match(/^\/workflows\/([^/]+)\/([^/]+)\/?$/);
  if (!m || !EXPERT_WORKFLOW_TABS.includes(m[2])) return null;
  return m[1];
}

/** Whether `path` is an Expert-only page (one a Lab-view user reaches only by a link). */
export function isExpertOnly(path: string): boolean {
  if (expertRunTab(path) !== null || expertWorkflowTab(path) !== null) return true;
  return EXPERT_PAGES.some((p) => path === p || path.startsWith(`${p}/`));
}

/**
 * The Lab page that stands for `path`: the run's own page for one of its Expert tabs, the
 * pipeline's Run tab for one of its Expert tabs, Home for any other Expert-only page, and `path`
 * itself everywhere else (both views share every other URL).
 */
export function labEquivalent(path: string): string {
  const run = expertRunTab(path);
  if (run !== null) {
    const lab = Object.keys(LAB_RUN_PAGES).find((page) => LAB_RUN_PAGES[page] === run.tab);
    return lab ? `/runs/${run.run}/${lab}` : `/runs/${run.run}`;
  }
  const version = expertWorkflowTab(path);
  if (version !== null) return `/workflows/${version}/run`;
  if (isExpertOnly(path)) return '/';
  return path;
}

/**
 * The Expert page that stands for `path`: the Expert tab a run's Lab page stands for (its file
 * list's Outputs tab), and `path` itself everywhere else.
 */
export function expertEquivalent(path: string): string {
  return expertOfLabRunPage(path) ?? path;
}

/**
 * Where toggling to `to` from `path` goes. Switching to Expert keeps the URL (every page exists
 * there) but for a run's Lab file list, which goes to its Outputs tab; switching to Lab from an
 * Expert-only page goes to its Lab equivalent.
 */
export function toggleTarget(path: string, to: ViewMode): string {
  return to === 'expert' ? expertEquivalent(path) : labEquivalent(path);
}
