// The words the UI uses, per view. Lab view speaks the bench's language (Analysis, Pipeline,
// Results); Expert keeps the engine's (Run, Workflow, Outputs). URLs and API fields never change.
// Only words a page chooses by view live here; a page that is Lab-only or Expert-only just says it.

import type { ViewMode } from '$lib/view/viewRoutes';

const WORDS = {
  runs: { lab: 'Analyses', expert: 'Runs' },
  runsLower: { lab: 'analyses', expert: 'runs' },
  runLower: { lab: 'analysis', expert: 'run' },
  workflows: { lab: 'Pipelines', expert: 'Workflows' },
  workflowLower: { lab: 'pipeline', expert: 'workflow' },
  newRun: { lab: 'Start an analysis', expert: 'New run' },
  home: { lab: 'My work', expert: 'Home' },
  succeeded: { lab: 'finished', expert: 'succeeded' },
} as const;

export type Word = keyof typeof WORDS;

/** The word for `w` in `mode`. */
export function t(w: Word, mode: ViewMode): string {
  return WORDS[w][mode];
}
