import { describe, expect, it, vi } from 'vitest';
import type { WorkflowSummary } from '$lib/api';

let answer: () => Promise<WorkflowSummary[]> = async () => [];
vi.mock('$lib/api', () => ({ listWorkflows: () => answer() }));

const { WorkflowCatalog } = await import('./catalog.svelte');
const { pinName } = await import('./version');
const { prefs } = await import('$lib/state/localPrefs.svelte');

/** A version of `name` in project `p`, registered at `t` (newest first wins `latest`). */
const wf = (name: string, t: number, over: Partial<WorkflowSummary> = {}) =>
  ({
    id: `${name}:${t}`,
    project: 'p',
    name,
    engine: 'snakemake',
    git_sha: 'abc',
    created_at_micros: t,
    ...over,
  }) as WorkflowSummary;
const released = { release: { released: true } } as Partial<WorkflowSummary>;
/** Let settled promises run their handlers. */
const flush = async () => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

describe('WorkflowCatalog', () => {
  it('groups the versions, keeps what the page offers, then applies the Developer filter', async () => {
    answer = async () => [
      wf('a', 2),
      wf('a', 1, released),
      wf('b', 1),
      wf('c', 1, { ...released, hidden: true }),
    ];
    const catalog = new WorkflowCatalog((g) => !g.latest.hidden);
    expect(catalog.loading).toBe(true);
    catalog.load();
    await flush();
    expect(catalog.loading).toBe(false);
    expect(catalog.error).toBeNull();
    expect(catalog.groups.map((g) => g.name)).toEqual(['a', 'b']);
    prefs.showDeveloper = false;
    expect(catalog.filtered.shown.map((g) => g.name)).toEqual(['a']);
    expect(catalog.filtered.hiddenByFilter).toBe(1);
    prefs.showDeveloper = true;
    expect(catalog.filtered.shown.map((g) => g.name)).toEqual(['a', 'b']);
    prefs.showDeveloper = false;
  });

  it('says why it could not load, and clears that on the next load', async () => {
    answer = async () => {
      throw new Error('down');
    };
    const catalog = new WorkflowCatalog();
    catalog.load();
    await flush();
    expect(catalog.error).toBe('down');
    answer = async () => [wf('a', 1)];
    catalog.load();
    expect(catalog.error).toBeNull();
    await flush();
    expect(catalog.groups).toHaveLength(1);
  });
});

describe('a pin', () => {
  const key = (name: string) => JSON.stringify(['p', name]);
  it('is known to be gone only once the list loaded: not while it loads, nor when it failed', async () => {
    answer = async () => [wf('a', 1), wf('h', 1, { hidden: true })];
    const catalog = new WorkflowCatalog();
    expect(catalog.pinStatus(key('a'))).toBe('unknown');
    catalog.load();
    await flush();
    expect(catalog.pinStatus(key('a'))).toBe('listed');
    expect(catalog.pinStatus(key('h'))).toBe('gone');
    expect(catalog.pinStatus(key('x'))).toBe('gone');
    answer = async () => {
      throw new Error('down');
    };
    catalog.load();
    await flush();
    expect(catalog.pinStatus(key('x'))).toBe('unknown');
    // An empty catalog is an answer too: every pin is gone.
    answer = async () => [];
    catalog.load();
    await flush();
    expect(catalog.pinStatus(key('a'))).toBe('gone');
  });

  it('is named from its key when the catalog does not list it', () => {
    expect(pinName(key('rnaseq'))).toBe('rnaseq');
    expect(pinName('not json')).toBe('not json');
  });
});
