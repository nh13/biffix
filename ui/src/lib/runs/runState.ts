// Whether a run is still in flight. The server decides (`RunSummary.terminal`, from its own run
// lifecycle), so the UI keeps no list of state names that could drift from the backend's: a state
// the server adds later is handled by the flag it comes with.

/** The lifecycle fact every run shape (`RunSummary`, `RunDetail`) carries. */
export interface RunLifecycle {
  terminal?: boolean;
}

/**
 * A run the server reports as still in flight: keep polling it and show it live. Only an explicit
 * `terminal: false` counts, so a run without the fact is not live (never polled forever).
 */
export const isLiveRun = (run: RunLifecycle): boolean => run.terminal === false;

/** A run that no longer changes — stop live-polling it. */
export const isTerminalRun = (run: RunLifecycle): boolean => !isLiveRun(run);

/** How a run state reads in a badge, where the state's own name is not already words. */
const RUN_STATE_LABELS: Readonly<Record<string, string>> = {
  preparing_images: 'preparing images',
};

/** A run state in words, for its badge (an unknown state shows as itself). */
export const runStateLabel = (state: string): string =>
  Object.hasOwn(RUN_STATE_LABELS, state) ? RUN_STATE_LABELS[state] : state;
