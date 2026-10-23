import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DataFile, DataFindResult, DataFolderPage, DataSource } from '$lib/api';

// Each API call hands back a promise the test settles, in whatever order it likes.
interface Pending<T> {
  kind: 'list' | 'find';
  arg: string;
  /** The page cursor a listing asked for. */
  cursor?: string;
  /** The project the request named. */
  project?: string | null;
  resolve: (v: T) => void;
  reject: (e: Error) => void;
}
const calls: Pending<unknown>[] = [];
function pending<T>(
  kind: 'list' | 'find',
  arg: string,
  cursor?: string,
  project?: string | null,
): Promise<T> {
  return new Promise<T>((resolve, reject) =>
    calls.push({ kind, arg, cursor, project, resolve: resolve as (v: unknown) => void, reject }),
  );
}

vi.mock('$lib/api', () => ({
  listDataFolder: (
    _s: string,
    _p: string,
    opts: { startsWith?: string; cursor?: string; project?: string | null } = {},
  ) => pending<DataFolderPage>('list', opts.startsWith ?? '', opts.cursor, opts.project),
  findDataFiles: (_s: string, _p: string, q: string, project?: string | null) =>
    pending<DataFindResult>('find', q, undefined, project),
}));

const { FolderListing } = await import('./folderListing.svelte');

const source: DataSource = {
  id: 'core',
  name: 'Core',
  kind: 'data',
  uri: 's3://b/',
  writable: false,
};
const file = (name: string) => ({ name, path: name, uri: `s3://b/${name}` }) as DataFile;
const listing = (...names: string[]) =>
  ({ folders: [], files: names.map(file), next_cursor: null }) as unknown as DataFolderPage;
/** A page of a listing, with the cursor of the page after it. */
const page = (next: string | null, ...names: string[]) =>
  ({
    folders: names.map((n) => ({ name: `${n}-dir`, path: `${n}-dir/` })),
    files: names.map(file),
    next_cursor: next,
  }) as unknown as DataFolderPage;
const found = (...names: string[]) =>
  ({ files: names.map(file), truncated: false }) as unknown as DataFindResult;
const names = (files: DataFile[] | null) => files?.map((f) => f.name) ?? null;
/** Let settled promises run their handlers. */
const flush = () => vi.advanceTimersByTimeAsync(0);

/** A listing opened at the root, its first listing answered. */
async function opened() {
  const l = new FolderListing();
  l.open(source, '');
  calls[0].resolve(listing('first'));
  await flush();
  return l;
}

beforeEach(() => {
  calls.length = 0;
  vi.useFakeTimers();
});
afterEach(() => vi.useRealTimers());

describe('FolderListing', () => {
  it('keeps the search when Enter follows typing within the debounce', async () => {
    const l = await opened();
    l.startsWith = 'P01';
    l.filterChanged();
    await vi.advanceTimersByTimeAsync(100);
    l.search();
    // The search is slower than what is left of the debounce.
    await vi.advanceTimersByTimeAsync(300);
    expect(calls.slice(1).map((c) => c.kind)).toEqual(['find']);
    calls[1].resolve(found('P01_R1.fq.gz'));
    await flush();
    expect(names(l.searchResults)).toEqual(['P01_R1.fq.gz']);
    expect(l.loading).toBe(false);
  });

  it('never lets a slow first listing overwrite a newer one', async () => {
    const l = new FolderListing();
    l.open(source, '');
    l.open(source, 'a/');
    calls[1].resolve(listing('second'));
    await flush();
    calls[0].resolve(listing('first'));
    await flush();
    expect(names(l.files)).toEqual(['second']);
    expect(l.loading).toBe(false);
  });

  it('drops a slow search once the filter text changes', async () => {
    const l = await opened();
    l.startsWith = 'P0';
    l.search();
    l.startsWith = 'P01';
    l.filterChanged();
    // The old search answers before the debounced re-list starts.
    calls[1].resolve(found('P02_R1.fq.gz'));
    await flush();
    expect(l.searchResults).toBeNull();
    await vi.advanceTimersByTimeAsync(250);
    expect(calls[2]).toMatchObject({ kind: 'list', arg: 'P01' });
    calls[2].resolve(listing('P01'));
    await flush();
    expect(l.searchResults).toBeNull();
    expect(names(l.files)).toEqual(['P01']);
    expect(l.loading).toBe(false);
  });

  it('retries a failed search as a search', async () => {
    const l = await opened();
    l.startsWith = 'P01';
    l.search();
    calls[1].reject(new Error('boom'));
    await flush();
    expect(l.error).toBe('boom');
    expect(l.searchResults).toBeNull();
    l.retry();
    expect(calls[2]).toMatchObject({ kind: 'find', arg: 'P01' });
    calls[2].resolve(found('P01_R1.fq.gz'));
    await flush();
    expect(l.error).toBeNull();
    expect(names(l.searchResults)).toEqual(['P01_R1.fq.gz']);
  });

  it('retries a failed listing as a listing', async () => {
    const l = new FolderListing();
    l.open(source, '');
    calls[0].reject(new Error('down'));
    await flush();
    expect(l.error).toBe('down');
    l.retry();
    expect(calls[1]).toMatchObject({ kind: 'list' });
    calls[1].resolve(listing('x'));
    await flush();
    expect(l.error).toBeNull();
    expect(names(l.files)).toEqual(['x']);
  });

  it('pages: each next page follows the cursor and adds to the one before', async () => {
    const l = new FolderListing();
    l.open(source, '');
    expect(calls[0].cursor).toBeUndefined();
    calls[0].resolve(page('c1', 'a', 'b'));
    await flush();
    expect(l.cursor).toBe('c1');
    l.load(true);
    expect(calls[1]).toMatchObject({ kind: 'list', cursor: 'c1' });
    calls[1].resolve(page('c2', 'c'));
    await flush();
    expect(names(l.files)).toEqual(['a', 'b', 'c']);
    expect(l.folders.map((f) => f.name)).toEqual(['a-dir', 'b-dir', 'c-dir']);
    expect(l.cursor).toBe('c2');
    l.load(true);
    expect(calls[2].cursor).toBe('c2');
    calls[2].resolve(page(null, 'd'));
    await flush();
    expect(names(l.files)).toEqual(['a', 'b', 'c', 'd']);
    expect(l.cursor).toBeNull();
  });

  it('retries a failed next page as the next page', async () => {
    const l = new FolderListing();
    l.open(source, '');
    calls[0].resolve(page('c1', 'a'));
    await flush();
    l.load(true);
    calls[1].reject(new Error('down'));
    await flush();
    expect(l.error).toBe('down');
    expect(names(l.files)).toEqual(['a']);
    l.retry();
    expect(calls[2]).toMatchObject({ kind: 'list', cursor: 'c1' });
    calls[2].resolve(page(null, 'b'));
    await flush();
    expect(names(l.files)).toEqual(['a', 'b']);
  });

  it('opening another folder drops a next page still on its way', async () => {
    const l = new FolderListing();
    l.open(source, '');
    calls[0].resolve(page('c1', 'a'));
    await flush();
    l.load(true);
    l.open(source, 'x/');
    expect(calls[2].cursor).toBeUndefined();
    calls[2].resolve(page(null, 'x1'));
    await flush();
    calls[1].resolve(page(null, 'b'));
    await flush();
    expect(names(l.files)).toEqual(['x1']);
    expect(l.cursor).toBeNull();
  });

  it('opening a folder cancels a pending filter re-list', async () => {
    const l = await opened();
    l.startsWith = 'P';
    l.filterChanged();
    l.open(source, 'a/');
    await vi.advanceTimersByTimeAsync(300);
    expect(calls.slice(1).map((c) => `${c.kind}:${c.arg}`)).toEqual(['list:']);
  });

  it('names the project it was opened in on every listing, search and page, and none by default', async () => {
    const l = new FolderListing();
    l.open(source, '', 'imported');
    calls[0].resolve(page('c1', 'a'));
    await flush();
    l.load(true);
    l.startsWith = 'a';
    void l.search();
    expect(calls.map((c) => c.project)).toEqual(['imported', 'imported', 'imported']);
    l.open(source, '');
    expect(calls[3].project).toBeNull();
  });

  it("never lets a slow listing of the project left behind overwrite the new project's", async () => {
    const l = new FolderListing();
    l.open(source, '', 'imported');
    // The same source id in the default project, opened before the first answers.
    l.open(source, '');
    calls[1].resolve(listing('default'));
    await flush();
    calls[0].resolve(listing('imported'));
    await flush();
    expect(l.project).toBeNull();
    expect(names(l.files)).toEqual(['default']);
    expect(l.loading).toBe(false);
  });
});
