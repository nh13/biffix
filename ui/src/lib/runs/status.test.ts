import { describe, expect, it } from 'vitest';
import { statusLine } from './status';

const run = (over: Record<string, unknown>) =>
  ({
    state: 'running',
    completed: null,
    total: null,
    tasks_succeeded: 0,
    tasks_failed: 0,
    submitted_at: null,
    completed_at: null,
    ...over,
  }) as Parameters<typeof statusLine>[0];

describe('statusLine', () => {
  it('reads the waiting states in words', () => {
    expect(statusLine(run({ state: 'preparing_images' }))).toMatchObject({
      tone: 'waiting',
      label: 'Getting ready',
    });
    expect(statusLine(run({ state: 'submitted' })).label).toBe('Waiting to start');
  });

  it('counts steps against a known total, else what finished', () => {
    expect(statusLine(run({ completed: 7, total: 12 })).detail).toBe('7 of 12 steps done');
    // Cache hits are already among the succeeded steps, so they are not counted again.
    expect(statusLine(run({ tasks_succeeded: 2, tasks_cached: 1 })).detail).toBe('2 steps done');
    expect(statusLine(run({ total: 12, tasks_succeeded: 3, tasks_cached: 3 })).detail).toBe(
      '3 of 12 steps done',
    );
    expect(statusLine(run({ tasks_succeeded: 1 })).detail).toBe('1 step done');
    expect(statusLine(run({})).detail).toBe('');
  });

  it('says how long a finished run took', () => {
    const line = statusLine(
      run({ state: 'succeeded', submitted_at: 0, completed_at: 3 * 3600 + 720 }),
    );
    expect(line).toEqual({ tone: 'done', label: 'Finished', detail: 'took 3h 12m' });
  });

  it('flags a failed run as needing attention, with what failed', () => {
    expect(statusLine(run({ state: 'failed', tasks_failed: 1 }))).toEqual({
      tone: 'attention',
      label: 'Needs attention',
      detail: '1 step failed',
    });
    expect(statusLine(run({ state: 'failed' })).detail).toBe('stopped before its steps ran');
  });

  it('counts samples when the run has them', () => {
    const samples = (total: number, done: number, failed: number, declared = true) => ({
      total,
      done,
      failed,
      declared,
    });
    expect(statusLine(run({ total: 40, completed: 10, samples: samples(12, 9, 0) })).detail).toBe(
      '9 of 12 samples done',
    );
    expect(statusLine(run({ samples: samples(12, 9, 1) })).detail).toBe(
      '9 of 12 samples done · 1 failed',
    );
    expect(statusLine(run({ samples: samples(3, 1, 0, false) })).detail).toBe(
      '1 done · 3 samples seen so far',
    );
    expect(statusLine(run({ state: 'failed', samples: samples(12, 11, 1) })).detail).toBe(
      '1 of 12 samples failed · 11 finished',
    );
    expect(statusLine(run({ state: 'failed', samples: samples(4, 2, 1, false) })).detail).toBe(
      '1 sample failed · 2 finished',
    );
    expect(statusLine(run({ state: 'failed', samples: samples(39, 0, 1) })).detail).toBe(
      '1 of 39 samples failed',
    );
    const done = statusLine(
      run({ state: 'succeeded', submitted_at: 0, completed_at: 3600, samples: samples(12, 12, 0) }),
    );
    expect(done.detail).toBe('12 samples · took 1h 0m');
    // No samples: steps, as before.
    expect(statusLine(run({ completed: 7, total: 12, samples: null })).detail).toBe(
      '7 of 12 steps done',
    );
  });

  it('shows an unknown state as words rather than failing', () => {
    expect(statusLine(run({ state: 'paused_for_review' })).label).toBe('paused for review');
    expect(statusLine(run({ state: 'cancelled' })).tone).toBe('stopped');
  });

  it('says how far a cancelled run got', () => {
    expect(statusLine(run({ state: 'cancelled', completed: 1, total: 8 }))).toEqual({
      tone: 'stopped',
      label: 'Cancelled',
      detail: '1 of 8 steps done',
    });
    expect(statusLine(run({ state: 'cancelled' })).detail).toBe('');
  });
});
