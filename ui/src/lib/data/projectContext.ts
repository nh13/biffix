// The project a launch form's file pickers browse and upload into: the version's own (each project
// has its own bucket and data sources). The form sets it once; every picker below it, however deep
// (a field, a samplesheet's cells), reads it, instead of the project being threaded through each.

import { getContext, setContext } from 'svelte';

const KEY = Symbol('data-project');

/** Make `project()` the project the file pickers below this component use (`null`: the default). */
export function setDataProject(project: () => string | null): void {
  setContext(KEY, project);
}

/** The project set by an enclosing form, or `null` (the deploy's default project) outside one. */
export function getDataProject(): () => string | null {
  return getContext<(() => string | null) | undefined>(KEY) ?? (() => null);
}
