import { describe, expect, it } from 'vitest';

import { progressView, type ProgressFacts } from './progress';

const base: ProgressFacts = {
  terminal: false,
  completed: null,
  total: null,
  tasks_observed: 0,
  tasks_succeeded: 0,
  tasks_running: 0,
  tasks_queued: 0,
  tasks_failed: 0,
  tasks_cached: 0,
};

describe('progressView', () => {
  it('never shows 100% before the plan is done', () => {
    const v = progressView({ ...base, completed: 199, total: 200 });
    expect(v.determinate).toBe(true);
    expect(v.pct).toBe(99);
    expect(progressView({ ...base, completed: 200, total: 200 }).pct).toBe(100);
    expect(progressView({ ...base, completed: 1, total: 3 }).pct).toBe(33);
  });

  it('clamps an engine count that overshoots its plan', () => {
    expect(progressView({ ...base, completed: 250, total: 200 }).pct).toBe(100);
  });

  it('falls back to observed successes when the engine count is missing', () => {
    const v = progressView({ ...base, total: 4, tasks_observed: 2, tasks_succeeded: 2 });
    expect(v.done).toBe(2);
    expect(v.pct).toBe(50);
  });

  it('is indeterminate without a plan total', () => {
    const v = progressView({ ...base, total: null, completed: 3 });
    expect(v.determinate).toBe(false);
    expect(v.pct).toBe(0);
    expect(progressView({ ...base, total: 0 }).determinate).toBe(false);
  });

  it('breaks the observed rows into buckets that sum to the total', () => {
    const v = progressView({
      ...base,
      tasks_observed: 10,
      tasks_succeeded: 4,
      tasks_running: 2,
      tasks_queued: 1,
      tasks_failed: 1,
      tasks_cached: 3,
    });
    expect([v.succeeded, v.running, v.queued, v.failed, v.unclassified]).toEqual([4, 2, 1, 1, 2]);
    expect(v.succeeded + v.running + v.queued + v.failed + v.unclassified).toBe(v.observed);
    expect(v.cached).toBe(3);
    // Inconsistent counts never make a negative bucket.
    expect(progressView({ ...base, tasks_observed: 1, tasks_succeeded: 3 }).unclassified).toBe(0);
  });

  it('shows no jobs for a run that ended without a plan or a task', () => {
    expect(progressView({ ...base, terminal: true }).noJobs).toBe(true);
    // Still live: it may yet schedule some.
    expect(progressView(base).noJobs).toBe(false);
    expect(progressView({ ...base, terminal: true, tasks_observed: 1 }).noJobs).toBe(false);
    expect(progressView({ ...base, terminal: true, total: 3 }).noJobs).toBe(false);
  });

  it("takes terminality from the server's flag only", () => {
    expect(progressView({ ...base, terminal: true }).terminal).toBe(true);
    expect(progressView(base).terminal).toBe(false);
    expect(progressView({ ...base, terminal: undefined }).terminal).toBe(true);
  });
});
