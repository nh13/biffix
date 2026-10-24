import { describe, expect, it } from 'vitest';
import type { DataSource } from '#lib/api/index.js';
import { expertPresentation, labPresentation } from './presentation';

const project = {
  id: 'project',
  name: 'Project data',
  kind: 'project',
  uri: 's3://b/',
  description: 'Your files',
  hidden_prefixes: ['biffix/'],
  browsable_exceptions: ['biffix/runs/'],
} as unknown as DataSource;
const refs = { ...project, id: 'refs', kind: 'references', description: null } as DataSource;
const nameOf = (id: string) => (id === 'r1' ? 'rnaseq' : null);
const labels = (steps: { label: string }[]) => steps.map((s) => s.label);

describe('labPresentation', () => {
  const p = labPresentation;
  it('adds the analyses and uploads, and no Expert extras', () => {
    expect(p).toMatchObject({
      view: 'lab',
      copyFolderPath: false,
      unknownSourceDetails: null,
      analyses: expect.any(Object),
      uploads: expect.any(Object),
    });
  });
  it('names sources plainly, titled by their description', () => {
    expect(p.sourceKind(project)).toBe('your project');
    expect(p.sourceKind(refs)).toBe('genomes');
    expect(p.sourceTitle(project)).toBe('Your files');
    expect(p.sourceTitle(refs)).toBeUndefined();
    expect(p.sourcesError('boom')).toBe("Couldn't load your data: boom");
  });
  it('words searches and empty folders', () => {
    expect(p.searchSummary(1, false)).toBe('1 match in this folder and the folders inside it.');
    expect(p.searchSummary(1200, true)).toBe(
      '1,200 matches in this folder and the folders inside it (stopped early: try a longer name).',
    );
    expect(p.empty(false, true)).toBe('This folder is empty. Upload files to add some.');
    expect(p.empty(true, false)).toBe('Nothing matches.');
  });
  it('says why a result cannot open, and how an upload is going', () => {
    expect(p.analyses?.cannotOpen.notRecorded('a.bam')).toMatch(
      /^a\.bam can't be opened here: its analysis didn't record it/,
    );
    expect(p.analyses?.cannotOpen.notFound('a.bam')).toMatch(
      /^a\.bam can't be opened here: biffix couldn't find it/,
    );
    expect(p.uploads?.uploading(1, 'uploads/x/')).toBe(
      'Uploading 1 file to uploads/x/: they keep going while you work.',
    );
    expect(p.uploads?.stopped('uploads/x/')).toBe('The upload to uploads/x/ stopped.');
    expect(p.uploads?.held('uploads/x/')).toBe(
      'Some files for uploads/x/ are paused or stopped: resume or retry them in Uploads.',
    );
  });
  it('names analyses and hides hidden folders in the trail', () => {
    expect(labels(p.trail(project, 'biffix/runs/r1/', nameOf))).toEqual([
      'Project data',
      'Analysis results',
      'rnaseq',
    ]);
  });
  it('says an unknown source in a sentence of its own', () => {
    expect(p.unknownSource).toMatch(/^This folder doesn't exist, .*\.$/);
  });
});

describe('expertPresentation', () => {
  const p = expertPresentation;
  it('adds a folder Copy path and source details, and none of the Lab additions', () => {
    expect(p).toMatchObject({
      view: 'expert',
      copyFolderPath: true,
      unknownSourceDetails: expect.any(Function),
      analyses: null,
      uploads: null,
    });
    expect(p.unknownSourceDetails?.('nope')).toBe('There is no data source “nope”.');
  });
  it('titles sources by their URI and marks references', () => {
    expect(p.sourceKind(project)).toBeNull();
    expect(p.sourceKind(refs)).toBe('references');
    expect(p.sourceTitle(project)).toBe('s3://b/');
    expect(p.sourcesError('boom')).toBe("Couldn't load data sources: boom");
  });
  it('words searches and empty folders', () => {
    expect(p.searchSummary(1, false)).toBe('1 match under this folder.');
    expect(p.searchSummary(2, true)).toBe(
      '2 matches under this folder (stopped early — narrow the search).',
    );
    expect(p.empty(true, true)).toBe('Nothing here.');
  });
  it("names every folder in the trail by its own name, never an analysis's", () => {
    const trail = p.trail(project, 'biffix/runs/r1/', nameOf);
    expect(labels(trail)).toEqual(['Project data', 'biffix', 'runs', 'r1']);
    expect(trail[1]).toMatchObject({ kind: 'hidden' });
  });
  it('says an unknown source in a sentence of its own', () => {
    expect(p.unknownSource).toMatch(/^This folder doesn't exist, .*\.$/);
  });
});
