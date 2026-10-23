import { describe, expect, it } from 'vitest';
import type { TaskAttempt } from '$lib/api';
import { fmtDate } from '$lib/util/format';
import { attemptStory, machineWords, tryText } from './attemptStory';

const attempt = (over: Partial<TaskAttempt>): TaskAttempt => ({
  batch_job_id: 'job',
  state: 'failed',
  ...over,
});
const outcome = (over: Partial<TaskAttempt>) => attemptStory([attempt(over)])[0].outcome;

describe('attemptStory', () => {
  it('tells a spot reclaim then an out-of-memory failure, without any log', () => {
    const lines = attemptStory([
      attempt({
        attempt: 1,
        started_at: 1000,
        finished_at: 1000 + 40 * 60,
        spot_warned_at: 1000 + 39 * 60,
        host: { capacity: 'spot' },
        requested_memory_mib: 8192,
      }),
      attempt({
        attempt: 2,
        started_at: 5000,
        finished_at: 5000 + 70 * 60,
        termination_category: 'oom',
        exit_code: 137,
        host: { capacity: 'on_demand' },
        requested_memory_mib: 16384,
      }),
    ]);
    expect(lines).toEqual([
      {
        label: 'Try 1',
        started: `started ${fmtDate(1000)}`,
        where: 'on a low-cost (spot) machine',
        memory: 'with 8.0 GiB of memory',
        outcome: 'AWS took the machine back after 40 min',
        retried: true,
        tone: 'bad',
      },
      {
        label: 'Try 2',
        started: `started ${fmtDate(5000)}`,
        where: 'on a standard (on-demand) machine',
        memory: 'with 16 GiB of memory',
        outcome: 'ran out of memory after 1 h 10 min',
        retried: false,
        tone: 'bad',
      },
    ]);
    expect(tryText(lines[0])).toBe(
      `Try 1 · started ${fmtDate(1000)} · on a low-cost (spot) machine · with 8.0 GiB of memory · ` +
        'AWS took the machine back after 40 min · tried again automatically',
    );
  });

  it('words every way a try can end, the category spelt in any case', () => {
    expect(outcome({ termination_category: 'OOM' })).toBe('ran out of memory');
    expect(outcome({ termination_category: 'spot' })).toBe('AWS took the machine back');
    expect(outcome({ termination_category: 'host_terminated' })).toBe(
      'the machine stopped unexpectedly',
    );
    expect(outcome({ termination_category: 'missing_output' })).toBe(
      'finished without writing the files it should have',
    );
    expect(outcome({ exit_code: 137 })).toBe('was stopped (often a sign of running out of memory)');
    expect(outcome({ termination_category: 'app_error', exit_code: 2 })).toBe(
      'stopped with an error (exit code 2)',
    );
    expect(outcome({})).toBe('stopped with an error');
    expect(outcome({ state: 'succeeded' })).toBe('finished');
  });

  it('says what a try still going is doing, and numbers unnumbered tries by place', () => {
    const lines = attemptStory([
      attempt({ state: 'failed', termination_category: 'spot' }),
      attempt({ state: 'running' }),
      attempt({ state: 'queued' }),
    ]);
    expect(lines.map((l) => [l.label, l.outcome, l.retried, l.tone])).toEqual([
      ['Try 1', 'AWS took the machine back', true, 'bad'],
      ['Try 2', 'running now', false, 'live'],
      ['Try 3', 'waiting for a machine', false, 'live'],
    ]);
  });

  it('ranks a spot warning as the failure diagnosis and the Expert view do', () => {
    // Memory first: a try warned of a reclaim may still have run out of memory before it.
    expect(outcome({ spot_warned_at: 5, termination_category: 'oom' })).toBe('ran out of memory');
    // Then the reclaim, ahead of what the lost machine left behind.
    expect(outcome({ spot_warned_at: 5, termination_category: 'missing_output' })).toBe(
      'AWS took the machine back',
    );
    expect(outcome({ spot_warned_at: 5, termination_category: 'host_terminated' })).toBe(
      'AWS took the machine back',
    );
    expect(outcome({ spot_warned_at: 5 })).toBe('AWS took the machine back');
    expect(outcome({ spot_warned_at: 5, termination_category: 'spot' })).toBe(
      'AWS took the machine back',
    );
  });

  it('says a try left open when the analysis ended reported no result, never that it runs', () => {
    // A stranded task's attempt keeps its last state (`run_ended` is the task's, not the try's).
    const lines = attemptStory(
      [
        attempt({ state: 'failed', exit_code: 1 }),
        attempt({ state: 'running' }),
        attempt({ state: 'queued' }),
      ],
      { runEnded: true },
    );
    expect(lines.map((l) => [l.outcome, l.tone])).toEqual([
      ['stopped with an error (exit code 1)', 'bad'],
      ['no result reported (the analysis ended)', 'bad'],
      ['no result reported (the analysis ended)', 'bad'],
    ]);
  });

  it('leaves out what it does not know', () => {
    const [line] = attemptStory([attempt({ started_at: 10, finished_at: 5 })]);
    expect(line.where).toBeNull();
    expect(line.memory).toBeNull();
    // A finish before its start is not a duration.
    expect(line.outcome).toBe('stopped with an error');
    expect(attemptStory([])).toEqual([]);
  });
});

describe('machineWords', () => {
  it('names spot and on-demand in lab words, and another lifecycle as itself', () => {
    expect(machineWords('spot')).toBe('on a low-cost (spot) machine');
    expect(machineWords('on_demand')).toBe('on a standard (on-demand) machine');
    expect(machineWords('capacity_block')).toBe('on a capacity-block machine');
    expect(machineWords(null)).toBeNull();
    expect(machineWords('')).toBeNull();
  });
});
