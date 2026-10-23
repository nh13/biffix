import { describe, expect, it, vi } from 'vitest';
import type { RunSummary } from '$lib/api';
import { idBatches, NAME_TTL_MS, RunNames } from './runNames.svelte';

const summary = (run_id: string, run_name: string | null = null) =>
  ({ run_id, run_name, workflow_name: 'rnaseq', submitted_at: null }) as unknown as RunSummary;
const flush = () => new Promise((r) => setTimeout(r, 0));

describe('idBatches', () => {
  it('drops blanks and repeats, and splits into batches of at most the size', () => {
    expect(idBatches(['a', ' ', 'b', 'a', 'c'], 2)).toEqual([['a', 'b'], ['c']]);
    expect(idBatches([])).toEqual([]);
    expect(idBatches(Array.from({ length: 401 }, (_, i) => `r${i}`)).map((b) => b.length)).toEqual([
      200, 200, 1,
    ]);
  });
});

describe('RunNames', () => {
  it('asks once per id, names the ones the server lists, and leaves the rest unnamed', async () => {
    const fetch = vi.fn(async (ids: string[]) =>
      ids.filter((id) => id !== 'gone').map((id) => summary(id, `name of ${id}`)),
    );
    const names = new RunNames(fetch);
    names.want(['a', 'gone']);
    names.want(['a']);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(names.name('a')).toBeNull();
    await flush();
    expect(names.name('a')).toBe('name of a');
    expect(names.summary('a')?.run_id).toBe('a');
    expect(names.name('gone')).toBeNull();
    names.want(['a', 'gone', 'b']);
    expect(fetch).toHaveBeenLastCalledWith(['b']);
  });

  it('a run without a name of its own gets the derived one', async () => {
    const names = new RunNames(async (ids) => ids.map((id) => summary(id)));
    names.want(['r1']);
    await flush();
    expect(names.name('r1')).toBe('rnaseq');
  });

  it('asks again for ids whose request failed', async () => {
    const fetch = vi.fn().mockRejectedValueOnce(new Error('down'));
    fetch.mockResolvedValue([summary('a', 'A')]);
    const names = new RunNames(fetch);
    names.want(['a']);
    await flush();
    expect(names.name('a')).toBeNull();
    names.want(['a']);
    await flush();
    expect(names.name('a')).toBe('A');
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('trims an id before asking, so a padded one is asked once', async () => {
    const fetch = vi.fn(async (ids: string[]) => ids.map((id) => summary(id, id.toUpperCase())));
    const names = new RunNames(fetch);
    names.want([' a ']);
    names.want(['a', ' a']);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalledWith(['a']);
    await flush();
    expect(names.name(' a ')).toBe('A');
  });

  it('a rename shows at once and an older answer does not undo it', async () => {
    let answer!: (runs: RunSummary[]) => void;
    const fetch = vi.fn(() => new Promise<RunSummary[]>((r) => (answer = r)));
    let now = 1000;
    const names = new RunNames(fetch, () => now);
    names.want(['a']);
    now = 2000;
    names.put(summary('a', 'Liver batch 2'));
    expect(names.name('a')).toBe('Liver batch 2');
    // The request sent before the rename answers with the old name.
    answer([summary('a', 'Old')]);
    await flush();
    expect(names.name('a')).toBe('Liver batch 2');
  });

  it('asks again once what it knows is old: a rename elsewhere, a run not listed before', async () => {
    let name = 'Old';
    const fetch = vi.fn(async (ids: string[]) => ids.map((id) => summary(id, name)));
    let now = 0;
    const names = new RunNames(fetch, () => now);
    names.want(['a']);
    await flush();
    expect(names.name('a')).toBe('Old');
    name = 'New';
    now = NAME_TTL_MS / 2;
    names.want(['a']);
    expect(fetch).toHaveBeenCalledTimes(1);
    now = NAME_TTL_MS + 1;
    names.want(['a']);
    expect(fetch).toHaveBeenCalledTimes(2);
    await flush();
    expect(names.name('a')).toBe('New');
  });
});
