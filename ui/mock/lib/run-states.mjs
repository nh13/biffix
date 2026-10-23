// Run-state groupings — mirror of the backend's `RunState` (`is_terminal`), which the server serves
// as each run's `terminal` flag. Keep in sync if a run state is added there.
export const ACTIVE = new Set(['preparing_images', 'submitted', 'running']);
export const TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);
