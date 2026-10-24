import { describe, expect, it } from 'vitest';
import { layout } from '#lib/runs/dag/layout.js';
import { fmtSpan, liveTimeFacts, stepChips, stepProgress, timeFacts } from './liveProgress';

const counts = (over: Record<string, number | null> = {}) => ({
  completed: null,
  total: null,
  tasks_observed: 0,
  tasks_succeeded: 0,
  tasks_cached: 0,
  ...over,
});

describe('stepProgress', () => {
  it('counts against the plan when the engine reported one', () => {
    expect(stepProgress(counts({ completed: 3, total: 12, tasks_observed: 5 }))).toEqual({
      fraction: 0.25,
      growing: false,
      text: '3 of 12 steps done',
    });
  });

  it('clamps an engine count past its plan, and falls back to successes without one', () => {
    expect(stepProgress(counts({ completed: 14, total: 12 }))?.fraction).toBe(1);
    expect(stepProgress(counts({ total: 4, tasks_succeeded: 1 }))?.text).toBe('1 of 4 steps done');
  });

  it('counts against the steps seen so far, plus one, while the total grows', () => {
    const p = stepProgress(counts({ tasks_observed: 5, tasks_succeeded: 2 }));
    expect(p).toEqual({ fraction: 2 / 6, growing: true, text: '2 of 5+ steps done' });
    // Every known step done: still short of the end, as more may come.
    expect(stepProgress(counts({ tasks_observed: 3, tasks_succeeded: 3 }))?.fraction).toBe(0.75);
  });

  it('is unknown before any step is', () => {
    expect(stepProgress(counts())).toBeNull();
    expect(stepProgress(counts({ total: 0 }))).toBeNull();
  });
});

const node = (id: string, n: Partial<Record<string, number>> = {}) => ({
  id,
  pending: 0,
  running: 0,
  succeeded: 0,
  failed: 0,
  cached: 0,
  ...n,
});

describe('stepChips', () => {
  it('orders steps after what they depend on, and says how each is doing', () => {
    // The server sorts nodes by id; the graph puts SAY before REVERSE.
    const chips = stepChips({
      nodes: [
        node('MULTIQC'),
        node('REVERSE', { running: 3, succeeded: 1 }),
        node('SAY', { succeeded: 2 }),
      ],
      edges: [
        ['SAY', 'REVERSE'],
        ['REVERSE', 'MULTIQC'],
      ],
    });
    expect(chips).toEqual([
      { group: 'SAY', state: 'done', done: 2, total: 2 },
      { group: 'REVERSE', state: 'running', done: 1, total: 4 },
      { group: 'MULTIQC', state: 'none', done: 0, total: 0 },
    ]);
  });

  it('marks failed and waiting steps, and keeps unordered steps in the server order', () => {
    const chips = stepChips({
      nodes: [node('a', { failed: 1, running: 1 }), node('b', { pending: 2 })],
      edges: [],
    });
    expect(chips.map((c) => [c.group, c.state])).toEqual([
      ['a', 'failed'],
      ['b', 'waiting'],
    ]);
  });

  it('survives edges to unknown steps, self-loops and cycles, and an empty graph', () => {
    const chips = stepChips({
      nodes: [node('x'), node('y')],
      edges: [
        ['x', 'y'],
        ['y', 'x'],
        ['x', 'x'],
        ['x', 'ghost'],
      ],
    });
    // Every step once, whatever order a cycle leaves.
    expect(chips.map((c) => c.group).sort()).toEqual(['x', 'y']);
    expect(stepChips(null)).toEqual([]);
    expect(stepChips({ nodes: [], edges: [] })).toEqual([]);
  });
});

describe('fmtSpan', () => {
  it('says a span in minutes, then hours and minutes, then hours', () => {
    expect(fmtSpan(20)).toBe('under a minute');
    expect(fmtSpan(40 * 60)).toBe('40 min');
    expect(fmtSpan(70 * 60)).toBe('1 h 10 min');
    expect(fmtSpan(2 * 3600)).toBe('2 h');
    expect(fmtSpan(26 * 3600 + 100)).toBe('26 h');
    expect(fmtSpan(-5)).toBe('under a minute');
  });
});

describe('stepChips and the DAG tab', () => {
  it('lists the steps in the order the graph draws them', () => {
    const ids = ['a', 'b', 'c', 'd', 'e'];
    const edges: [string, string][] = [
      ['a', 'c'],
      ['b', 'c'],
      ['c', 'd'],
      ['b', 'e'],
    ];
    const chips = stepChips({ nodes: ids.map((id) => node(id)), edges });
    expect(chips.map((c) => c.group)).toEqual(layout(ids, edges).nodes.map((n) => n.id));
  });
});

describe('liveTimeFacts', () => {
  const typical = { hours_median: 0.5, usd_median: null, runs: 3 };
  it('counts from launch, not submit: a wait for images is not run time', () => {
    // Submitted 40 min ago, launched 10 min ago.
    const now = 10_000;
    const run = { state: 'running', launched_at_micros: (now - 600) * 1e6 };
    expect(liveTimeFacts(run, now, typical)).toEqual({
      elapsed: 'Running for 10 min',
      usual: 'usually ~30 min · about 20 min left',
    });
  });
  it('launched but not on a machine yet, does not claim it is running', () => {
    const run = { state: 'submitted', launched_at_micros: 400 * 1e6 };
    expect(liveTimeFacts(run, 1000, null)?.elapsed).toBe('Waiting for a machine for 10 min');
  });
  it('before launch, says it is waiting to start, with no countdown', () => {
    expect(
      liveTimeFacts({ state: 'preparing_images', launched_at_micros: null }, 1, typical),
    ).toEqual({
      elapsed: 'Waiting to start: getting its software ready',
      usual: 'usually takes ~30 min once started',
    });
    expect(liveTimeFacts({ state: 'submitted' }, 1, null)).toEqual({
      elapsed: 'Waiting to start: queued for a machine',
      usual: null,
    });
  });
});

describe('timeFacts', () => {
  const typical = { hours_median: 2.5, usd_median: 14, runs: 6 };

  it('says only the time so far without past runs to go by', () => {
    expect(timeFacts(1000, 1000 + 600, null)).toEqual({
      elapsed: 'Running for 10 min',
      usual: null,
    });
    expect(timeFacts(1000, 1600, { hours_median: 0, runs: 2 })?.usual).toBeNull();
  });

  it('counts down from the usual time', () => {
    expect(timeFacts(0, 3600, typical)).toEqual({
      elapsed: 'Running for 1 h',
      usual: 'usually ~2.5 h · about 1 h 30 min left',
    });
  });

  it('says it is taking longer than usual rather than counting below zero', () => {
    expect(timeFacts(0, 3 * 3600, typical)?.usual).toBe(
      'usually ~2.5 h · taking longer than usual',
    );
    expect(timeFacts(0, 2.5 * 3600 - 30, typical)?.usual).toBe(
      'usually ~2.5 h · taking longer than usual',
    );
  });

  it('is nothing for a run with no start', () => {
    expect(timeFacts(null, 100, typical)).toBeNull();
  });
});
