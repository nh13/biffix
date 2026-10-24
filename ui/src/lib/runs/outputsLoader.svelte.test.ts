import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { RunOutputPage, RunOutput } from '#lib/api/index.js';

// Each request is recorded; `rows` and `pending` say what the next ones answer.
let rows: RunOutput[] = [];
let pending = 0;
let fail = false;
const calls: { limit?: number; task?: string; roles?: string[] }[] = [];

const list = async (
  _runId: string,
  opts: { limit?: number; task?: string; roles?: string[] } = {},
): Promise<RunOutputPage> => {
  calls.push({ limit: opts.limit, task: opts.task, roles: opts.roles });
  if (fail) throw new Error('down');
  return {
    outputs: opts.limit === 1 ? rows.slice(0, 1) : rows,
    total: rows.length,
    next_cursor: null,
    enrichment_pending: pending,
  } as unknown as RunOutputPage;
};

vi.mock('#lib/api/index.js', () => ({
  listOutputs: list,
  listAllOutputs: async (runId: string, opts: object) => {
    const page = await list(runId, { ...opts, limit: 2000 });
    return { outputs: page.outputs, total: page.total, last: page, truncated: false };
  },
}));

const { OutputsLoader } = await import('./outputsLoader.svelte');

const out = (id: string) => ({ id }) as RunOutput;
/** Let settled promises run their handlers. */
const flush = async () => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

beforeEach(() => {
  vi.useFakeTimers();
  rows = [out('a')];
  pending = 0;
  fail = false;
  calls.length = 0;
});
afterEach(() => vi.useRealTimers());

describe('OutputsLoader', () => {
  it('reads an ended run once', async () => {
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false });
    await flush();
    expect(files.loading).toBe(false);
    expect(files.outputs.map((o) => o.id)).toEqual(['a']);
    expect(files.total).toBe(1);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(1);
    stop();
  });

  it('re-reads a live run every 16 s until stopped', async () => {
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: true });
    await flush();
    rows = [out('a'), out('b')];
    await vi.advanceTimersByTimeAsync(16_000);
    expect(files.outputs.map((o) => o.id)).toEqual(['a', 'b']);
    expect(calls).toHaveLength(2);
    stop();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(2);
  });

  it('between full reads, only probes the count, reading the list again when it changed', async () => {
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: true, probeEvery: 4 });
    await flush();
    await vi.advanceTimersByTimeAsync(16_000); // beat 1: count unchanged
    expect(calls.map((c) => c.limit)).toEqual([2000, 1]);
    rows = [out('a'), out('b')];
    await vi.advanceTimersByTimeAsync(16_000); // beat 2: count changed, so the list
    expect(calls.map((c) => c.limit)).toEqual([2000, 1, 1, 2000]);
    expect(files.total).toBe(2);
    await vi.advanceTimersByTimeAsync(16_000); // beat 3: unchanged
    await vi.advanceTimersByTimeAsync(16_000); // beat 4: a full read anyway
    expect(calls.map((c) => c.limit)).toEqual([2000, 1, 1, 2000, 1, 2000]);
    stop();
  });

  it('keeps asking an ended run quickly until its sizes are in', async () => {
    pending = 3;
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false, waitForSizes: true });
    await flush();
    expect(files.pending).toBe(3);
    expect(files.sizing).toBe(true);
    await vi.advanceTimersByTimeAsync(2_500);
    expect(calls).toHaveLength(2);
    pending = 0;
    await vi.advanceTimersByTimeAsync(2_500);
    expect(files.pending).toBe(0);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(3);
    stop();
  });

  it('reads the whole list each time an ended run is asked for its sizes, probing or not', async () => {
    // A probe only compares the count, which sizing never changes.
    pending = 2;
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false, probeEvery: 4, waitForSizes: true });
    await flush();
    await vi.advanceTimersByTimeAsync(2_500);
    pending = 0;
    await vi.advanceTimersByTimeAsync(2_500);
    expect(calls.map((c) => c.limit)).toEqual([2000, 2000, 2000]);
    expect(files.pending).toBe(0);
    stop();
  });

  it('gives up on sizes that never come after 40 tries, and says it stopped waiting', async () => {
    pending = 3;
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false, waitForSizes: true });
    await flush();
    await vi.advanceTimersByTimeAsync(2_500 * 60);
    expect(calls).toHaveLength(40);
    expect(files.pending).toBe(3);
    expect(files.sizing).toBe(false);
    stop();
  });

  it('stops asking a failing ended run after 40 tries', async () => {
    pending = 3;
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false, waitForSizes: true });
    await flush();
    expect(files.sizing).toBe(true);
    fail = true;
    await vi.advanceTimersByTimeAsync(2_500 * 60);
    expect(calls).toHaveLength(40);
    expect(files.sizing).toBe(false);
    stop();
  });

  it('fails an ended run that cannot be read at all', async () => {
    fail = true;
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false, waitForSizes: true });
    await flush();
    expect(files.error).toBe('down');
    expect(files.sizing).toBe(false);
    stop();
  });

  it('passes the query on, and clear() forgets the listing', async () => {
    const files = new OutputsLoader();
    const stop = files.watch({ runId: 'r', live: false, query: { task: 't', roles: ['*'] } });
    await flush();
    expect(calls[0]).toMatchObject({ task: 't', roles: ['*'] });
    files.clear();
    expect(files.outputs).toEqual([]);
    expect(files.total).toBe(0);
    stop();
  });
});
