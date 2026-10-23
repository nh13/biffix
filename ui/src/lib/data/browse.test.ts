import { describe, expect, it } from 'vitest';
import { dataBrowserHref, dataSpotOf, hasDataFolders, hiddenIn } from './browse';
import { dataLocation } from './paths';

// The project source's rule as the server serves it (`DataSource::view`).
const PROJECT_RULE = {
  hidden_prefixes: ['biffix/', 'workflow-sources/'],
  browsable_exceptions: ['biffix/runs/'],
};

const SOURCES = [
  { id: 'project', kind: 'project', uri: 's3://proj/', ...PROJECT_RULE },
  { id: 'core', kind: 'data', uri: 's3://seq-core/deliveries/' },
  // A source inside another's bucket: the deeper one holds its keys.
  { id: 'batch', kind: 'data', uri: 's3://seq-core/deliveries/2026/' },
];

/** Where the Data page lands for `href` (its source, folder and file). */
function lands(href: string) {
  const url = new URL(href, 'http://x');
  const rest = decodeURIComponent(url.pathname.replace(/^\/data\//, ''));
  return dataLocation(url, rest);
}

describe('dataSpotOf', () => {
  it('finds a run output in the project bucket: its folder, with the file open', () => {
    expect(dataSpotOf('s3://proj/biffix/runs/r1/results/a.bam', SOURCES)).toEqual({
      sourceId: 'project',
      path: 'biffix/runs/r1/results/',
      file: 'a.bam',
    });
  });

  it('opens a folder (an output prefix) as a folder', () => {
    expect(dataSpotOf('s3://proj/biffix/runs/r1/', SOURCES)).toEqual({
      sourceId: 'project',
      path: 'biffix/runs/r1/',
      file: null,
    });
    expect(dataSpotOf('s3://proj/top.csv', SOURCES)).toEqual({
      sourceId: 'project',
      path: '',
      file: 'top.csv',
    });
  });

  it('picks the deepest source that holds it', () => {
    expect(dataSpotOf('s3://seq-core/deliveries/2026/b12/x.fq.gz', SOURCES)?.sourceId).toBe(
      'batch',
    );
    expect(dataSpotOf('s3://seq-core/deliveries/2025/x.fq.gz', SOURCES)).toEqual({
      sourceId: 'core',
      path: '2025/',
      file: 'x.fq.gz',
    });
  });

  it('is null outside every source of the project', () => {
    // Another bucket (a custom output prefix elsewhere), or outside a source's prefix.
    expect(dataSpotOf('s3://elsewhere/results/a.bam', SOURCES)).toBeNull();
    expect(dataSpotOf('s3://seq-core/private/x.txt', SOURCES)).toBeNull();
    // A bucket whose name only starts like a source's.
    expect(dataSpotOf('s3://proj-archive/a.txt', SOURCES)).toBeNull();
    expect(dataSpotOf('s3://proj/a.txt', [])).toBeNull();
  });

  it('is null under a prefix the Data page hides', () => {
    expect(dataSpotOf('s3://proj/biffix/cache/snakemake/x', SOURCES)).toBeNull();
    expect(dataSpotOf('s3://proj/workflow-sources/v1/source.tar.gz', SOURCES)).toBeNull();
    // Only the project bucket hides them.
    expect(dataSpotOf('s3://seq-core/deliveries/biffix/x.txt', SOURCES)?.sourceId).toBe('core');
  });

  it('is null for what is not an s3 URI', () => {
    for (const uri of [null, undefined, '', 'proj/a.txt', 's3://', 's3://proj']) {
      expect(dataSpotOf(uri, SOURCES)).toBeNull();
    }
  });
});

describe('hiddenIn', () => {
  it("applies the source's own rule: biffix's prefixes of the project bucket, but a run's folder", () => {
    const project = PROJECT_RULE;
    expect(hiddenIn(project, 'biffix/')).toBe(true);
    expect(hiddenIn(project, 'biffix/cache/snakemake/')).toBe(true);
    expect(hiddenIn(project, 'workflow-sources/x')).toBe(true);
    expect(hiddenIn(project, 'biffix/runs/')).toBe(false);
    expect(hiddenIn(project, 'biffix/runs/r1/a.txt')).toBe(false);
    expect(hiddenIn(project, 'uploads/a.txt')).toBe(false);
    // A source that hides nothing (no rule) hides nothing.
    expect(hiddenIn({}, 'biffix/x')).toBe(false);
    expect(hiddenIn({ hidden_prefixes: [], browsable_exceptions: [] }, 'biffix/x')).toBe(false);
  });
});

describe('dataBrowserHref', () => {
  it('links the Data page at the file, in the run’s project', () => {
    const href = dataBrowserHref('s3://proj/biffix/runs/r1/results/a b.bam', SOURCES, 'genomics');
    expect(href).toBe(
      '/data/project?project=genomics#path=biffix%2Fruns%2Fr1%2Fresults%2F&file=a+b.bam',
    );
    expect(lands(href!)).toEqual({
      sourceId: 'project',
      path: 'biffix/runs/r1/results/',
      file: 'a b.bam',
    });
  });

  it('links a folder without a file, and the default project without a query', () => {
    const href = dataBrowserHref('s3://proj/biffix/runs/r1/', SOURCES, null);
    expect(href).toBe('/data/project#path=biffix%2Fruns%2Fr1%2F');
    expect(lands(href!)).toEqual({ sourceId: 'project', path: 'biffix/runs/r1/', file: null });
  });

  it('gives none where the Data page cannot show it', () => {
    expect(dataBrowserHref('s3://elsewhere/a.bam', SOURCES, 'genomics')).toBeNull();
    expect(dataBrowserHref('s3://proj/biffix/cache/x', SOURCES, 'genomics')).toBeNull();
  });
});

describe('hasDataFolders', () => {
  it('counts only data sources, not the project bucket or references', () => {
    expect(hasDataFolders([])).toBe(false);
    expect(hasDataFolders([{ kind: 'project' }, { kind: 'references' }])).toBe(false);
    expect(hasDataFolders([{ kind: 'project' }, { kind: 'data' }])).toBe(true);
  });
});
