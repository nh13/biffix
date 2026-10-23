import { describe, expect, it } from 'vitest';
import {
  runFolderId,
  runOfPath,
  sourceKindLabel,
  topShortcuts,
  uploadsHere,
} from './labPresentation';

const project = {
  name: 'Project data',
  writable: true,
  hidden_prefixes: ['biffix/', 'workflow-sources/'],
  browsable_exceptions: ['biffix/runs/'],
};
const core = { name: 'Sequencing core', writable: false, hidden_prefixes: [] as string[] };

describe('runOfPath / runFolderId', () => {
  it("finds the run whose results folder a path is in, in a source that shows runs' folders", () => {
    expect(runOfPath(project, 'biffix/runs/run_1/star/x.bam')).toEqual({
      runId: 'run_1',
      rest: 'star/x.bam',
    });
    expect(runOfPath(project, 'biffix/runs/run_1/')).toEqual({ runId: 'run_1', rest: '' });
    expect(runOfPath(project, 'biffix/runs/')).toBeNull();
    expect(runOfPath(project, 'uploads/x')).toBeNull();
    expect(runOfPath(core, 'biffix/runs/run_1/x')).toBeNull();
  });
  it('names only the folder itself as a run folder', () => {
    expect(runFolderId(project, 'biffix/runs/run_1/')).toBe('run_1');
    expect(runFolderId(project, 'biffix/runs/run_1/star/')).toBeNull();
  });
});

describe('topShortcuts', () => {
  it("offers the analyses' results at the project's top only", () => {
    expect(topShortcuts(project, '').map((s) => s.path)).toEqual(['biffix/runs/']);
    expect(topShortcuts(project, 'uploads/')).toEqual([]);
    expect(topShortcuts(core, '')).toEqual([]);
  });
});

describe('uploadsHere', () => {
  it('takes uploads at the top of a writable source and under uploads/', () => {
    expect(uploadsHere(project, '')).toBe(true);
    expect(uploadsHere(project, 'uploads/2026/')).toBe(true);
    expect(uploadsHere(project, 'biffix/runs/run_1/')).toBe(false);
    expect(uploadsHere(core, '')).toBe(false);
  });
});

describe('sourceKindLabel', () => {
  it('names a source kind plainly', () => {
    expect(sourceKindLabel('project')).toBe('your project');
    expect(sourceKindLabel('references')).toBe('genomes');
    expect(sourceKindLabel('data')).toBeNull();
  });
});
