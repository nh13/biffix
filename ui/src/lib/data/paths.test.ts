import { describe, expect, it } from 'vitest';
import {
  breadcrumbs,
  chosenProject,
  dataHref,
  dataLocation,
  dataProjectHref,
  folderOf,
  offersProjectChoice,
  requestedProject,
} from './paths';

const at = (href: string) => new URL(href, 'http://x');
/** What SvelteKit hands the Data page as `params.rest` for `href`: the path after `/data/`, decoded. */
const restOf = (href: string) =>
  at(href)
    .pathname.replace(/^\/data\/?/, '')
    .split('/')
    .map(decodeURIComponent)
    .join('/');
/** Where the Data page lands for `href`. */
const locate = (href: string) => dataLocation(at(href), restOf(href));

describe('breadcrumbs', () => {
  it('builds the trail from the root', () => {
    expect(breadcrumbs('Core', '2026/batch-12/')).toEqual([
      { label: 'Core', path: '' },
      { label: '2026', path: '2026/' },
      { label: 'batch-12', path: '2026/batch-12/' },
    ]);
    expect(breadcrumbs('Core', '')).toEqual([{ label: 'Core', path: '' }]);
  });
  it('keeps empty folder names, so each crumb opens the prefix it names', () => {
    expect(breadcrumbs('Core', 'a//b/').map((c) => c.path)).toEqual(['', 'a/', 'a//', 'a//b/']);
    expect(breadcrumbs('Core', 'a//').map((c) => c.path)).toEqual(['', 'a/', 'a//']);
  });
});

describe('Data page URLs', () => {
  it('keep folder and file names out of the path and the query', () => {
    for (const href of [
      dataHref('bench-data', 'smoke/1M/'),
      dataHref('core', 'P01 sample/', 'P01_R1.fastq.gz'),
    ]) {
      const url = at(href);
      expect(url.search).toBe('');
      expect(url.pathname).not.toMatch(/smoke|P01/);
    }
    expect(dataHref('project', '')).toBe('/data/project');
    expect(dataHref('bench-data', 'smoke/1M/')).toBe('/data/bench-data#path=smoke%2F1M%2F');
    expect(dataHref('core', 'a b/', 'x#1.tsv')).toBe('/data/core#path=a+b%2F&file=x%231.tsv');
  });
  it('round-trip any folder and file name, empty and dot segments included', () => {
    for (const [id, path, file] of [
      ['core', '100%_cov/', undefined],
      ['core', 'a b/c%20d/', 'x&y=z.tsv'],
      ['my data', 'x%41/', undefined],
      ['project', '', undefined],
      ['core', 'a//', undefined],
      ['core', 'a//b/', 'c.txt'],
      ['core', '../', undefined],
      ['core', './x/', undefined],
    ] as const)
      expect(locate(dataHref(id, path, file))).toEqual({
        sourceId: id,
        path,
        file: file ?? null,
      });
  });
  it('read the (already decoded) source id without decoding it twice', () => {
    expect(locate('/data/core%2F100%25_cov')).toEqual({
      sourceId: 'core/100%_cov',
      path: '',
      file: null,
    });
    expect(dataLocation(at('/data/50%25'), '50%')).toEqual({
      sourceId: '50%',
      path: '',
      file: null,
    });
    expect(locate('/data')).toEqual({ sourceId: null, path: '', file: null });
  });
});

describe('the Data page across projects', () => {
  const projects = [
    { id: 'imported', default: false },
    { id: 'genomics', default: true },
  ];
  it('offers a choice of project only on a deploy of more than one', () => {
    expect(offersProjectChoice(projects)).toBe(true);
    expect(offersProjectChoice([{ id: 'genomics', default: true }])).toBe(false);
    expect(offersProjectChoice([])).toBe(false);
    // Before (or without) the projects, the page browses as a deploy of one.
    expect(offersProjectChoice(null)).toBe(false);
  });
  it('names a project other than the default in the query, the folder still in the fragment', () => {
    const href = dataHref('bench-data', 'hg002/', 'HG002_R1.fastq.gz', 'imported');
    expect(href).toBe('/data/bench-data?project=imported#path=hg002%2F&file=HG002_R1.fastq.gz');
    expect(locate(href)).toEqual({
      sourceId: 'bench-data',
      path: 'hg002/',
      file: 'HG002_R1.fastq.gz',
    });
    expect(requestedProject(at(href))).toBe('imported');
    // The default project is named by leaving it out: links stay as they were.
    expect(dataHref('project', '', undefined, null)).toBe('/data/project');
    expect(requestedProject(at('/data/project'))).toBeNull();
    expect(requestedProject(at('/data?project=%20'))).toBeNull();
    expect(dataProjectHref('imported')).toBe('/data?project=imported');
    expect(dataProjectHref(null)).toBe('/data');
  });
  it('shows the requested project as chosen, else the default one', () => {
    expect(chosenProject('imported', projects)).toBe('imported');
    expect(chosenProject(null, projects)).toBe('genomics');
    expect(chosenProject(null, null)).toBeNull();
  });
});

describe('folderOf', () => {
  it('is the path up to its last slash', () => {
    expect(folderOf('a/b/c.txt')).toBe('a/b/');
    expect(folderOf('c.txt')).toBe('');
    expect(folderOf('a/b/')).toBe('a/b/');
  });
});
