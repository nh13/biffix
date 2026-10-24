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

const { ProjectSources } = await import('./projectSources.svelte');

const source = (id: string): DataSource => ({
  id,
  name: id,
  kind: 'data',
  uri: `s3://${id}/`,
  writable: false,
});
const ids = (sources: DataSource[] | null) => sources?.map((s) => s.id) ?? null;
/** Let settled promises run their handlers. */
const flush = () => new Promise((r) => setTimeout(r, 0));

describe('ProjectSources', () => {
  beforeEach(() => {
    calls.length = 0;
  });

  it("keeps the current project's sources when the project left behind answers last", async () => {
    const sources = new ProjectSources();
    sources.load('imported');
    // Switched back to the default project before the first answer.
    sources.load(null);
    expect(calls.map((c) => c.project)).toEqual(['imported', null]);
    calls[1].resolve([source('core')]);
    await flush();
    calls[0].resolve([source('bench-data')]);
    await flush();
    expect(ids(sources.sources(null))).toEqual(['core']);
    expect(sources.sources('imported')).toBeNull();
    expect(sources.error(null)).toBeNull();
  });

  it('drops a late failure for the project left behind', async () => {
    const sources = new ProjectSources();
    sources.load('imported');
    sources.load(null);
    calls[1].resolve([source('core')]);
    await flush();
    calls[0].reject(new Error('boom'));
    await flush();
    expect(ids(sources.sources(null))).toEqual(['core']);
    expect(sources.error(null)).toBeNull();
  });

  it('shows nothing for a project until its own answer lands, then its sources or error', async () => {
    const sources = new ProjectSources();
    sources.load(null);
    calls[0].resolve([source('core')]);
    await flush();
    sources.load('imported');
    // The default project's answer is never shown as the new project's.
    expect(sources.sources('imported')).toBeNull();
    calls[1].reject(new Error('project `imported` is not configured'));
    await flush();
    expect(sources.sources('imported')).toBeNull();
    expect(sources.error('imported')).toBe('project `imported` is not configured');
  });
});
