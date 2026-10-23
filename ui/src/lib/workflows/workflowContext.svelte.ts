// The workflow version a `/workflows/[id]` page shows, loaded by the route's layout and shared with
// its tab pages. The pure tab helpers live in `workflowNav.ts`.

import { getContext, setContext } from 'svelte';
import type { WorkflowDetail, WorkflowSummary } from '$lib/api';

export class WorkflowState {
  detail = $state<WorkflowDetail | null>(null);
  loading = $state(true);
  error = $state<string | null>(null);
  /** What kind of failure `error` is, for the error page's words. */
  errorKind = $state<import('$lib/components/errorPage').ErrorKind>('server');
  /** Bumped by the error page's Try again: the layout loads the version again. */
  loadAttempt = $state(0);
  /** Every version of the same (project, name), newest first. */
  versions = $state<WorkflowSummary[]>([]);
  /** Whether `versions` is still being loaded. */
  versionsLoading = $state(true);
  /** Why `versions` could not be loaded, if it could not. */
  versionsError = $state<string | null>(null);
}

const KEY = Symbol('workflow');

export function setWorkflowContext(wf: WorkflowState): WorkflowState {
  return setContext(KEY, wf);
}

export function getWorkflowContext(): WorkflowState {
  const wf = getContext<WorkflowState | undefined>(KEY);
  if (!wf) throw new Error('getWorkflowContext() outside a /workflows/[id] layout');
  return wf;
}
