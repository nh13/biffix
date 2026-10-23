import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { DataSource } from '#lib/api/index.js';

// Each request hands back a promise the test settles, in whatever order it likes.
interface Pending {
  project: string | null;
  resolve: (v: DataSource[]) => void;
  reject: (e: Error) => void;
}
const calls: Pending[] = [];

vi.mock('#lib/api/index.js', () => ({
  listDataSources: (project: string | null) =>
    new Promise<DataSource[]>((resolve, reject) => calls.push({ project, resolve, reject })),
}));

const { cachedDataSources, clearDataSourcesCache } = await import('./browse');
const { DataLinks } = await import('./dataLinks.svelte');

/** A project source over `bucket`, with the server's hide rule. */
const project = (bucket: string): DataSource => ({
  id: 'project',
  name: 'Project data',
  kind: 'project',
  uri: `s3://${bucket}/`,
  writable: false,
  hidden_prefixes: ['biffix/', 'workflow-sources/'],
  browsable_exceptions: ['biffix/runs/'],
});
/** Let settled promises run their handlers. */
const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  calls.length = 0;
  clearDataSourcesCache();
});

describe('cachedDataSources', () => {
  it('asks once per project, and asks again after a failure', async () => {
    const first = cachedDataSources('a');
    expect(cachedDataSources('a')).toBe(first);
    expect(calls).toHaveLength(1);
    calls[0].reject(new Error('down'));
    await expect(first).rejects.toThrow('down');
    // The failure is forgotten: the next call asks again, and that answer is kept.
    const second = cachedDataSources('a');
    expect(calls).toHaveLength(2);
    calls[1].resolve([project('a-bucket')]);
    expect((await second).map((s) => s.uri)).toEqual(['s3://a-bucket/']);
    expect(cachedDataSources('a')).toBe(second);
    expect(calls).toHaveLength(2);
  });
});

describe('DataLinks', () => {
  const RESULT = 's3://a-bucket/biffix/runs/r1/results/x.bam';

  it('links nothing until the sources load, then links into the project', async () => {
    const links = new DataLinks();
    links.load('a');
    expect(links.href(RESULT)).toBeNull();
    calls[0].resolve([project('a-bucket')]);
    await flush();
    expect(links.href(RESULT)).toBe(
      '/data/project?project=a#path=biffix%2Fruns%2Fr1%2Fresults%2F&file=x.bam',
    );
    // What the source hides, or another bucket, gets no link.
    expect(links.href('s3://a-bucket/biffix/cache/x')).toBeNull();
    expect(links.href('s3://elsewhere/x')).toBeNull();
  });

  it("never lets a stale answer for another project link this one's URIs (A → B → A)", async () => {
    const links = new DataLinks();
    links.load('a');
    calls[0].resolve([project('a-bucket')]);
    await flush();
    // Off to B, and back to A before B's answer lands.
    links.load('b');
    expect(links.href(RESULT)).toBeNull();
    links.load('a');
    // B's own bucket: had its answer landed for A, A's result would no longer resolve.
    expect(calls[1].project).toBe('b');
    calls[1].resolve([project('b-bucket')]);
    await flush();
    expect(links.href(RESULT)).toContain('project=a');
    expect(links.href(RESULT)).not.toContain('project=b');
    expect(links.href('s3://b-bucket/x')).toBeNull();
  });

  it("drops a slower answer for the project left behind (A's after B's)", async () => {
    const links = new DataLinks();
    links.load('a');
    links.load('b');
    calls[1].resolve([project('b-bucket')]);
    await flush();
    calls[0].resolve([project('a-bucket')]);
    await flush();
    expect(links.href(RESULT)).toBeNull();
    expect(links.href('s3://b-bucket/top.csv')).toBe('/data/project?project=b#file=top.csv');
  });

  it('links again once a failed load is retried', async () => {
    const links = new DataLinks();
    links.load('a');
    calls[0].reject(new Error('down'));
    await flush();
    expect(links.href(RESULT)).toBeNull();
    links.load('a');
    expect(calls).toHaveLength(2);
    calls[1].resolve([project('a-bucket')]);
    await flush();
    expect(links.href(RESULT)).not.toBeNull();
  });
});
