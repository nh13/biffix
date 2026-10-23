import { describe, expect, it } from 'vitest';
import type { RunSummary } from '#lib/api/index.js';
import { afterDismissal, sortRuns, type SortKey } from './runsSort';

const run = (run_id: string, over: Partial<RunSummary> = {}) =>
  ({ run_id, state: 'succeeded', ...over }) as RunSummary;

const a = run('r-a', {
  run_name: 'Beta',
  workflow_name: 'zeta-wf',
  workflow_title: 'Alpha pipeline',
  state: 'running',
  engine: 'snakemake',
  project: 'p2',
  submitted_at: 200,
  completed_at: null,
  submitter: 'u2',
});
const b = run('r-b', {
  run_name: 'alpha',
  workflow_name: 'alpha-wf',
  workflow_title: 'Zeta pipeline',
  state: 'failed',
  engine: 'nextflow',
  project: 'p1',
  submitted_at: 100,
  completed_at: 150,
  submitter: 'u1',
  submitter_label: 'Ada',
});
const c = run('r-c', { submitted_at: null, completed_at: null });

const ids = (rs: RunSummary[]) => rs.map((r) => r.run_id);
const sort = (key: SortKey, dir: 'asc' | 'desc' = 'asc', lab = false) =>
  ids(sortRuns([a, b, c], key, dir, { nameOf: (r) => r.run_name ?? r.run_id, lab }));

describe('sortRuns', () => {
  it('sorts each column both ways', () => {
    expect(sort('run')).toEqual(['r-a', 'r-b', 'r-c']);
    expect(sort('run', 'desc')).toEqual(['r-c', 'r-b', 'r-a']);
    // Names ignore case.
    expect(sort('name')).toEqual(['r-b', 'r-a', 'r-c']);
    expect(sort('state')).toEqual(['r-b', 'r-a', 'r-c']);
    expect(sort('engine')).toEqual(['r-c', 'r-b', 'r-a']);
    expect(sort('project')).toEqual(['r-c', 'r-b', 'r-a']);
    // The submitter as shown: its label when it has one.
    expect(sort('submitter')).toEqual(['r-c', 'r-b', 'r-a']);
  });

  it('sorts a time not known yet as the earliest', () => {
    expect(sort('submitted')).toEqual(['r-c', 'r-b', 'r-a']);
    expect(sort('submitted', 'desc')).toEqual(['r-a', 'r-b', 'r-c']);
    expect(sort('completed', 'desc')).toEqual(['r-b', 'r-a', 'r-c']);
  });

  it("sorts the workflow by the Lab view's pipeline title, or the Expert view's name", () => {
    expect(sort('workflow', 'asc', false)).toEqual(['r-c', 'r-b', 'r-a']);
    expect(sort('workflow', 'asc', true)).toEqual(['r-c', 'r-a', 'r-b']);
  });

  it('leaves the rows it is given as they are', () => {
    const rows = [a, b];
    sortRuns(rows, 'name', 'asc', { nameOf: (r) => r.run_id, lab: false });
    expect(ids(rows)).toEqual(['r-a', 'r-b']);
  });
});

describe('afterDismissal', () => {
  it('goes back to the first page only when this page emptied while more need attention', () => {
    expect(afterDismissal(0, 3, { index: 2, req: { after: 'c' } })).toBe('first-page');
    expect(afterDismissal(0, null, { index: 1, req: { before: 'c' } })).toBe('first-page');
    // Nothing left anywhere, rows still here, or already on the first page: refresh in place.
    expect(afterDismissal(0, 0, { index: 2, req: { after: 'c' } })).toBe('refresh');
    expect(afterDismissal(4, 3, { index: 2, req: { after: 'c' } })).toBe('refresh');
    expect(afterDismissal(0, 3, { index: 0, req: {} })).toBe('refresh');
  });
});
