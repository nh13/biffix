import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LogPage } from '$lib/api';
import { LogStreamModel } from './logStreamModel.svelte';

// Each fetch hands back a promise the test settles, in whatever order it likes.
interface Call {
  opts: { cursor?: string; direction?: 'forward' | 'backward'; limit?: number };
  resolve: (p: LogPage) => void;
  reject: (e: Error) => void;
}
let calls: Call[] = [];
const fetchPage = (opts: Call['opts']) =>
  new Promise<LogPage>((resolve, reject) => calls.push({ opts, resolve, reject }));

const page = (
  msgs: string[],
  fwd: string | null = 'f1',
  bwd: string | null = 'b1',
  stream: string | null = 's',
): LogPage => ({
  log_stream: stream,
  events: msgs.map((message, i) => ({ ts_ms: i, message })),
  next_forward_token: fwd,
  next_backward_token: bwd,
});
const noStream = (): LogPage => ({ events: [] }) as unknown as LogPage;
const msgs = (m: LogStreamModel) => m.lines.map((l) => l.message);
/** Let settled promises run their handlers. */
const flush = () => vi.advanceTimersByTimeAsync(0);

/** A model whose first (tail) page has been answered with `first`. */
async function opened(first = page(['a']), opts = {}) {
  const m = new LogStreamModel(fetchPage, opts);
  void m.loadFirst();
  calls[0].resolve(first);
  await flush();
  calls = [];
  return m;
}

beforeEach(() => {
  calls = [];
  vi.useFakeTimers();
});
afterEach(() => vi.useRealTimers());

describe('LogStreamModel first page', () => {
  it('keeps polling the first page while there is no stream, then tails its cursor', async () => {
    const m = await opened(noStream());
    expect(m.noStream).toBe(true);

    void m.tick(); // still queued
    expect(calls[0].opts).toEqual({ direction: 'backward', limit: 300 });
    calls[0].resolve(noStream());
    await flush();
    expect(m.noStream).toBe(true);

    void m.tick(); // the stream appeared
    calls[1].resolve(page(['a', 'b'], 'f1'));
    await flush();
    expect(m.noStream).toBe(false);
    expect(msgs(m)).toEqual(['a', 'b']);

    void m.tick(); // now the live tail
    expect(calls[2].opts).toEqual({ direction: 'forward', cursor: 'f1', limit: 300 });
  });

  it('retries a failed first load on the next tick, and a quiet failure keeps "no stream yet"', async () => {
    const m = new LogStreamModel(fetchPage);
    void m.loadFirst();
    calls[0].reject(new Error('503'));
    await flush();
    expect(m.error).toBe('503');

    void m.tick();
    calls[1].resolve(noStream());
    await flush();
    expect(m.error).toBeNull();
    expect(m.noStream).toBe(true);

    void m.tick(); // recorded but not created yet: a 404
    calls[2].reject(new Error('404'));
    await flush();
    expect(m.error).toBeNull(); // still "no logs yet", not an error
    expect(m.loading).toBe(false);
  });

  it('toggling loads the first page exactly once, in the new direction', async () => {
    const m = await opened();
    void m.toggleStart();
    expect(calls.map((c) => c.opts)).toEqual([{ direction: 'forward', limit: 300 }]);
    calls[0].resolve(page(['head']));
    await flush();
    expect(msgs(m)).toEqual(['head']);
    expect(m.atStart).toBe(true);
  });

  it('is last-writer-wins: a superseded toggle landing late is dropped', async () => {
    const m = await opened();
    void m.toggleStart(); // → from start
    void m.toggleStart(); // → back to the tail
    calls[1].resolve(page(['tail'], 'f2'));
    await flush();
    calls[0].resolve(page(['head'], 'f-head'));
    await flush();
    expect(msgs(m)).toEqual(['tail']);
    expect(m.fromStart).toBe(false);
    expect(m.atStart).toBe(false); // from the request's direction, not a stale one
    void m.tick();
    expect(calls[2].opts.cursor).toBe('f2');
  });
});

describe('LogStreamModel live tail', () => {
  it('never overlaps: a tick while a page is in flight joins it', async () => {
    const m = await opened();
    void m.tick();
    void m.tick();
    void m.tick();
    expect(calls).toHaveLength(1);
    calls[0].resolve(page(['b'], 'f2'));
    await flush();
    expect(msgs(m)).toEqual(['a', 'b']);
    void m.tick();
    expect(calls[1].opts.cursor).toBe('f2');
  });

  it('drops a tail page that lands after a toggle replaced the view', async () => {
    const m = await opened();
    void m.tick(); // tail poll in flight on f1
    void m.toggleStart();
    calls[1].resolve(page(['head'], 'f-head'));
    await flush();
    calls[0].resolve(page(['tail-new'], 'f-tail'));
    await flush();
    expect(msgs(m)).toEqual(['head']);
    void m.tick();
    expect(calls[2].opts.cursor).toBe('f-head');
  });

  it('a load more after a toggle fetches for the new view, not joining the old tail', async () => {
    const m = await opened();
    void m.tick(); // tail poll in flight on f1, for the view a toggle then replaces
    void m.toggleStart();
    calls[1].resolve(page(['head'], 'f-head'));
    await flush();
    void m.loadMore(); // while the old tail is still in flight
    expect(calls).toHaveLength(3);
    expect(calls[2].opts.cursor).toBe('f-head');
    calls[2].resolve(page(['next'], 'f-next'));
    await flush();
    // The old tail landing late neither appends nor clears the new one's state.
    calls[0].resolve(page(['tail-new'], 'f-tail'));
    await flush();
    expect(msgs(m)).toEqual(['head', 'next']);
    void m.tick();
    expect(calls[3].opts.cursor).toBe('f-next');
  });

  it('keeps the cursor when caught up (a forward token that stops advancing)', async () => {
    const m = await opened();
    void m.tick();
    calls[0].resolve(page([], null));
    await flush();
    void m.tick();
    expect(calls[1].opts.cursor).toBe('f1');
  });

  it('starts over when the stream behind the view changes', async () => {
    const m = await opened();
    void m.tick();
    calls[0].resolve(page(['other'], 'fx', 'bx', 'another-stream'));
    await flush();
    expect(msgs(m)).toEqual(['a']); // not appended
    expect(calls[1].opts).toEqual({ direction: 'backward', limit: 300 });
    calls[1].resolve(page(['new-a'], 'f9', 'b9', 'another-stream'));
    await flush();
    expect(msgs(m)).toEqual(['new-a']);
  });

  it('settles with a fetch after the in-flight one, and one more after a delay', async () => {
    const m = await opened();
    void m.tick(); // in flight when the task ends
    m.settle(1000);
    calls[0].resolve(page(['b'], 'f2'));
    await flush();
    expect(calls[1].opts.cursor).toBe('f2'); // the final lines, after the in-flight page
    calls[1].resolve(page(['error!'], 'f3'));
    await flush();
    expect(msgs(m)).toEqual(['a', 'b', 'error!']);
    await vi.advanceTimersByTimeAsync(1000);
    expect(calls[2].opts.cursor).toBe('f3'); // ingestion lag
  });

  it('a cancelled settle, or a disposed model, fetches nothing more', async () => {
    const m = await opened();
    const cancel = m.settle(1000);
    await flush();
    calls[0].resolve(page([], 'f1'));
    await flush();
    cancel();
    await vi.advanceTimersByTimeAsync(5000);
    expect(calls).toHaveLength(1);

    void m.tick();
    m.dispose();
    calls[1].resolve(page(['late'], 'f2'));
    await flush();
    expect(msgs(m)).toEqual(['a']);
    void m.tick();
    expect(calls).toHaveLength(2);
  });
});

describe('LogStreamModel load earlier', () => {
  it('follows empty pages with a new token: they are not the start', async () => {
    const m = await opened();
    void m.loadEarlier();
    calls[0].resolve(page([], 'x', 'b2'));
    await flush();
    expect(calls[1].opts).toEqual({ direction: 'backward', cursor: 'b2', limit: 300 });
    calls[1].resolve(page(['older'], 'x', 'b3'));
    await flush();
    expect(msgs(m)).toEqual(['older', 'a']);
    expect(m.atStart).toBe(false);
  });

  it('keeps the events of the last page (same token back) and marks the start', async () => {
    const m = await opened();
    void m.loadEarlier();
    calls[0].resolve(page(['first'], 'x', 'b1'));
    await flush();
    expect(msgs(m)).toEqual(['first', 'a']);
    expect(m.atStart).toBe(true);
  });

  it('a failure keeps the loaded lines and says so inline', async () => {
    const m = await opened();
    void m.loadEarlier();
    void m.loadEarlier(); // joined, not duplicated
    expect(calls).toHaveLength(1);
    calls[0].reject(new Error('throttled'));
    await flush();
    expect(msgs(m)).toEqual(['a']);
    expect(m.error).toBeNull();
    expect(m.earlierError).toBe('throttled');
    expect(m.loadingEarlier).toBe(false);
  });

  it('follows at most maxEmptyPages empty pages, then hands back to the person', async () => {
    const m = await opened(page(['a']), { maxEmptyPages: 2 });
    void m.loadEarlier();
    calls[0].resolve(page([], 'x', 'b2'));
    await flush();
    calls[1].resolve(page([], 'x', 'b3'));
    await flush();
    expect(calls).toHaveLength(2);
    expect(msgs(m)).toEqual(['a']);
    expect(m.atStart).toBe(false);
    expect(m.loadingEarlier).toBe(false);
    // The next "load earlier" goes on from where the last one stopped.
    void m.loadEarlier();
    expect(calls[2].opts).toEqual({ direction: 'backward', cursor: 'b3', limit: 300 });
  });

  it('drops an earlier page that lands after a toggle replaced the view', async () => {
    const m = await opened();
    void m.loadEarlier();
    void m.toggleStart();
    calls[1].resolve(page(['1', '2']));
    await flush();
    calls[0].resolve(page(['older'], 'x', 'b2'));
    await flush();
    expect(msgs(m)).toEqual(['1', '2']);
    expect(m.atStart).toBe(true);
    expect(m.earlierError).toBeNull();
  });
});

describe('LogStreamModel line cap', () => {
  it('at the tail, drops the oldest lines and withdraws "load earlier"', async () => {
    const m = await opened(page(['a', 'b']), { maxLines: 3 });
    void m.tick();
    calls[0].resolve(page(['c', 'd'], 'f2'));
    await flush();
    expect(msgs(m)).toEqual(['b', 'c', 'd']);
    expect(m.headTrimmed).toBe(true);
    void m.loadEarlier();
    expect(calls).toHaveLength(1); // no backward fetch: its cursor no longer adjoins the view
  });

  it('loading earlier lines past the cap keeps the newest and withdraws "load earlier"', async () => {
    const m = await opened(page(['c', 'd']), { maxLines: 3 });
    void m.loadEarlier();
    calls[0].resolve(page(['a', 'b'], 'x', 'b2'));
    await flush();
    expect(msgs(m)).toEqual(['b', 'c', 'd']);
    expect(m.headTrimmed).toBe(true);
    expect(m.atStart).toBe(false);
    void m.loadEarlier();
    expect(calls).toHaveLength(1);
  });

  it('read from the start, stops growing instead', async () => {
    const m = await opened(page(['a']), { maxLines: 3 });
    void m.toggleStart();
    calls[0].resolve(page(['1', '2']));
    await flush();
    void m.tick();
    calls[1].resolve(page(['3', '4'], 'f2'));
    await flush();
    expect(msgs(m)).toEqual(['1', '2', '3']);
    expect(m.tailCapped).toBe(true);
    void m.tick();
    expect(calls).toHaveLength(2);
  });
});
