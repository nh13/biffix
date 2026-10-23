import { describe, expect, it } from 'vitest';
import type { RunDetail } from '$lib/api';
import { activeRunTab, runMayHaveOutputs, runOutputsTotal, runTabHref, runTabs } from './runNav';

const run = (over: Partial<RunDetail> = {}): RunDetail =>
  ({
    run_id: 'run 1/x',
    state: 'running',
    tasks: [],
    params: { a: 1, b: 2 },
    tasks_observed: 5,
    outputs_total: 7,
    ...over,
  }) as unknown as RunDetail;

describe('runTabHref', () => {
  it('encodes the run id and puts Overview at the run URL', () => {
    expect(runTabHref('run 1/x', 'overview')).toBe('/runs/run%201%2Fx');
    expect(runTabHref('run 1/x', 'jobs')).toBe('/runs/run%201%2Fx/jobs');
    expect(runTabHref('r', 'params')).toBe('/runs/r/params');
  });
});

describe('runTabs', () => {
  it('lists every tab in order with counts', () => {
    const tabs = runTabs(run());
    expect(tabs.map((t) => t.label)).toEqual([
      'Overview',
      'Graph',
      'Jobs',
      'Outputs',
      'Reports',
      'Coordinator',
      'Cost',
      'Parameters',
    ]);
    const byId = Object.fromEntries(tabs.map((t) => [t.id, t]));
    expect(byId.jobs.count).toBe(5);
    expect(byId.outputs.count).toBe(7);
    expect(byId.params.count).toBe(2);
    expect(byId.overview.count).toBeNull();
    // Reports are counted once known, and dimmed when there are none.
    expect(byId.reports.count).toBeNull();
    expect(byId.reports.empty).toBe(false);
    const none = Object.fromEntries(runTabs(run(), 0).map((t) => [t.id, t]));
    expect(none.reports.empty).toBe(true);
    expect(runTabs(run(), 2).find((t) => t.id === 'reports')?.count).toBe(2);
  });

  it('marks tabs with nothing to show as empty', () => {
    const byId = Object.fromEntries(runTabs(run({ params: {} })).map((t) => [t.id, t]));
    expect(byId.graph.empty).toBe(true);
    expect(byId.coordinator.empty).toBe(true);
    expect(byId.cost.empty).toBe(true);
    expect(byId.params.empty).toBe(true);
    expect(byId.jobs.empty).toBe(false);
    const full = Object.fromEntries(
      runTabs(
        run({
          dag: { nodes: [{ id: 'a' }], edges: [] },
          coordinator_job_id: 'job-1',
          cost: { priced_jobs: 1 },
        } as unknown as Partial<RunDetail>),
      ).map((t) => [t.id, t]),
    );
    expect(full.graph.empty).toBe(false);
    expect(full.coordinator.empty).toBe(false);
    expect(full.cost.empty).toBe(false);
  });
});

describe('activeRunTab', () => {
  it('reads the tab from a pathname (the caller passes url.pathname: no query or fragment)', () => {
    expect(activeRunTab('/runs/r')).toBe('overview');
    expect(activeRunTab('/runs/r/')).toBe('overview');
    expect(activeRunTab('/runs/r/jobs')).toBe('jobs');
    expect(activeRunTab('/runs/run%201/params/')).toBe('params');
    expect(activeRunTab('/runs/r/nope')).toBe('overview');
  });
});

describe("a resumed run's outputs", () => {
  it('count what its outputs list does, once known, and only for a resumed run', () => {
    const resumed = run({ resumed_from: 'r0', outputs_total: 1 });
    // The list total (the files it reused included) once loaded; its own count until then.
    expect(runOutputsTotal(resumed, 3)).toBe(3);
    expect(runOutputsTotal(resumed)).toBe(1);
    expect(runTabs(resumed, null, 3).find((t) => t.id === 'outputs')?.count).toBe(3);
    // A run that resumed nothing counts what it registered.
    expect(runOutputsTotal(run(), 3)).toBe(7);
    expect(runTabs(run(), null, 3).find((t) => t.id === 'outputs')?.count).toBe(7);
  });

  it('may be there though the run registered none', () => {
    expect(runMayHaveOutputs(run({ outputs_total: 0 }))).toBe(false);
    expect(runMayHaveOutputs(run({ outputs_total: 2 }))).toBe(true);
    expect(runMayHaveOutputs(run({ outputs_total: 0, resumed_from: 'r0' }))).toBe(true);
  });
});
