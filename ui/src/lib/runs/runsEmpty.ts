// What the runs list says when it lists nothing.

import type { ListFilter } from './runsList.svelte';

/** The list's view and what narrows it. */
export interface EmptyListOf {
  lab: boolean;
  showEveryone: boolean;
  experimentScope: string;
  workflowScope: string;
  filter: ListFilter;
  search: string;
}

/**
 * Whether an empty list means the user has started nothing yet (the Lab view's own analyses,
 * unfiltered and in no experiment or workflow scope), so the page invites a first one, rather than
 * that nothing matches the filter.
 */
export function nothingStartedYet(l: EmptyListOf): boolean {
  return (
    l.lab &&
    !l.showEveryone &&
    !l.experimentScope &&
    !l.workflowScope &&
    l.filter === 'all' &&
    !l.search.trim()
  );
}
