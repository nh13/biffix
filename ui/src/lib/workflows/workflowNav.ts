// The tabs of a `/workflows/[id]` page and their URLs: pure, so any page can link to a workflow tab
// without importing the Svelte context module.

import type { WorkflowDetail } from '#lib/api/index.js';

export type WorkflowTab = 'run' | 'about' | 'parameters' | 'versions' | 'provenance';
/** Run first: it is the tab a biologist opens the workflow for. */
export const WORKFLOW_TABS: readonly WorkflowTab[] = [
  'run',
  'about',
  'parameters',
  'versions',
  'provenance',
];

/** The tabs `detail` has: About only when the version carries an About page. */
export function workflowTabs(detail: Pick<WorkflowDetail, 'about_markdown'>): WorkflowTab[] {
  return WORKFLOW_TABS.filter((t) => t !== 'about' || !!detail.about_markdown);
}

/** The page of version `id` (its own URL, which redirects to Run). */
export const workflowHref = (id: string): string => `/workflows/${encodeURIComponent(id)}`;

/** The href of `tab` for version `id`. */
export function workflowTabHref(id: string, tab: WorkflowTab): string {
  return `${workflowHref(id)}/${tab}`;
}

/** Which tab `pathname` is on (the version's own URL is Run). */
export function activeWorkflowTab(pathname: string): WorkflowTab {
  const segment = /^\/workflows\/[^/]+\/([^/]+)\/?$/.exec(pathname)?.[1] as WorkflowTab;
  return WORKFLOW_TABS.includes(segment) ? segment : 'run';
}
