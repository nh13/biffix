import { describe, expect, it } from 'vitest';
import type { RunOutput } from '$lib/api';
import {
  analysesShown,
  expertTrail,
  labTrail,
  opensAs,
  resultOfOther,
  resultOpen,
} from './dataBrowser';

const project = {
  name: 'Project data',
  hidden_prefixes: ['biffix/', 'workflow-sources/'],
  browsable_exceptions: ['biffix/runs/'],
};
const core = { name: 'Sequencing core', hidden_prefixes: [] as string[] };
const names: Record<string, string> = { run_1: 'rnaseq · batch-10' };
const nameOf = (id: string) => names[id] ?? null;

describe('expertTrail', () => {
  it('names every folder, links all but the last, and never links a hidden one', () => {
    expect(expertTrail(project, 'biffix/runs/run_1/star/')).toEqual([
      { label: 'Project data', path: '', kind: 'link' },
      { label: 'biffix', path: 'biffix/', kind: 'hidden' },
      { label: 'runs', path: 'biffix/runs/', kind: 'link' },
      { label: 'run_1', path: 'biffix/runs/run_1/', kind: 'link' },
      { label: 'star', path: 'biffix/runs/run_1/star/', kind: 'current' },
    ]);
  });
  it("is the source alone at the source's root", () => {
    expect(expertTrail(core, '')).toEqual([
      { label: 'Sequencing core', path: '', kind: 'current' },
    ]);
  });
});

describe('labTrail', () => {
  it('leaves out hidden folders and names the runs and an analysis', () => {
    expect(labTrail(project, 'biffix/runs/run_1/star/', nameOf)).toEqual([
      { label: 'Project data', path: '', kind: 'link' },
      { label: 'Analysis results', path: 'biffix/runs/', kind: 'link' },
      { label: 'rnaseq · batch-10', path: 'biffix/runs/run_1/', kind: 'link' },
      { label: 'star', path: 'biffix/runs/run_1/star/', kind: 'current' },
    ]);
  });
  it("falls back to an analysis's id, and is plain in a source without runs", () => {
    expect(labTrail(project, 'biffix/runs/run_2/', nameOf).map((c) => c.label)).toEqual([
      'Project data',
      'Analysis results',
      'run_2',
    ]);
    expect(labTrail(core, 'a/b/', nameOf)).toEqual([
      { label: 'Sequencing core', path: '', kind: 'link' },
      { label: 'a', path: 'a/', kind: 'link' },
      { label: 'b', path: 'a/b/', kind: 'current' },
    ]);
    expect(labTrail(core, '', nameOf)).toEqual([
      { label: 'Sequencing core', path: '', kind: 'current' },
    ]);
  });
  it('ends with the folder on show even when the source hides it', () => {
    expect(labTrail(project, 'workflow-sources/', nameOf)).toEqual([
      { label: 'Project data', path: '', kind: 'link' },
      { label: 'workflow-sources', path: 'workflow-sources/', kind: 'current' },
    ]);
    expect(labTrail(project, 'biffix/', nameOf).at(-1)).toEqual({
      label: 'biffix',
      path: 'biffix/',
      kind: 'current',
    });
  });
});

describe('analysesShown', () => {
  it("names the folder's own analysis, its run folders' and those of search results", () => {
    expect(
      [
        ...analysesShown(
          project,
          'biffix/runs/',
          [{ path: 'biffix/runs/run_1/' }, { path: 'biffix/runs/run_2/' }],
          null,
        ),
      ].sort(),
    ).toEqual(['run_1', 'run_2']);
    expect([...analysesShown(project, 'biffix/runs/run_3/x/', [], null)]).toEqual(['run_3']);
    expect([
      ...analysesShown(project, '', [{ path: 'uploads/' }], [{ path: 'biffix/runs/run_4/a.txt' }]),
    ]).toEqual(['run_4']);
  });
  it('names none in a source that shows no runs', () => {
    expect(
      analysesShown(core, 'biffix/runs/run_1/', [{ path: 'biffix/runs/run_1/a/' }], null).size,
    ).toBe(0);
  });
});

describe('resultOfOther', () => {
  it("is another analysis's id, never the folder's own", () => {
    expect(resultOfOther(project, '', 'biffix/runs/run_1/a.bam')).toBe('run_1');
    expect(resultOfOther(project, 'biffix/runs/run_1/', 'biffix/runs/run_1/a.bam')).toBeNull();
    expect(resultOfOther(project, 'biffix/runs/run_1/', 'biffix/runs/run_2/a.bam')).toBe('run_2');
    expect(resultOfOther(project, '', 'uploads/a.bam')).toBeNull();
  });
});

describe('opensAs', () => {
  const file = (name: string, path: string) => ({ name, path });
  it("opens an analysis's BAM/CRAM and FastQC report as its results", () => {
    expect(opensAs(project, file('a.bam', 'biffix/runs/r/a.bam'))).toBe('alignment');
    expect(opensAs(project, file('a.cram', 'biffix/runs/r/a.cram'))).toBe('alignment');
    expect(opensAs(project, file('a_fastqc.html', 'biffix/runs/r/a_fastqc.html'))).toBe('fastqc');
  });
  it("opens nothing else as a result, nor anything outside an analysis's folder", () => {
    expect(opensAs(project, file('a.bam.bai', 'biffix/runs/r/a.bam.bai'))).toBeNull();
    expect(opensAs(project, file('a.bam', 'uploads/a.bam'))).toBeNull();
    expect(opensAs(core, file('a.bam', 'biffix/runs/r/a.bam'))).toBeNull();
  });
});

describe('resultOpen', () => {
  const output = { output_id: 'o' } as unknown as RunOutput;
  const bam = { name: 'a.bam', viewer: null };
  const report = { name: 'a_fastqc.html', viewer: 'html' } as const;
  it('opens a found output as its kind', () => {
    expect(resultOpen({ output }, 'alignment', 'r1', bam)).toEqual({
      open: { kind: 'alignment', runId: 'r1', output, title: 'a.bam' },
    });
  });
  it('opens a recorded FastQC report in the FastQC viewer, not the file viewer', () => {
    expect(resultOpen({ output }, 'fastqc', 'r1', report)).toEqual({
      open: { kind: 'fastqc', runId: 'r1', output, title: 'a_fastqc.html' },
    });
  });
  it('opens an unrecorded FastQC report in the file viewer', () => {
    expect(resultOpen({ notRecorded: true }, 'fastqc', 'r1', report)).toEqual({ inViewer: true });
    expect(resultOpen({ truncated: true }, 'fastqc', 'r1', report)).toEqual({ inViewer: true });
  });
  it("says why it can't open the rest", () => {
    expect(resultOpen({ notRecorded: true }, 'alignment', 'r1', bam)).toEqual({
      cannot: 'notRecorded',
    });
    expect(resultOpen({ truncated: true }, 'alignment', 'r1', bam)).toEqual({
      cannot: 'notFound',
    });
    const noViewer = { name: 'a_fastqc.html', viewer: null };
    expect(resultOpen({ notRecorded: true }, 'fastqc', 'r1', noViewer)).toEqual({
      cannot: 'notRecorded',
    });
  });
});
