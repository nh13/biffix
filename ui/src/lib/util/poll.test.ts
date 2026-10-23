import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { PollNudge, startPoll } from './poll';

// A promise plus its resolve/reject, so a test can drive one fetch at a time deterministically.
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('startPoll', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('loads immediately, then again each interval', async () => {
    const values = [1, 2, 3];
    const fetch = vi.fn(() => Promise.resolve(values.shift() ?? 0));
    const onData = vi.fn();
    const stop = startPoll({ fetch, onData, intervalMs: 1000 });

    await vi.advanceTimersByTimeAsync(0); // let the immediate load resolve
    expect(onData).toHaveBeenLastCalledWith(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(onData).toHaveBeenLastCalledWith(2);
    await vi.advanceTimersByTimeAsync(1000);
    expect(onData).toHaveBeenLastCalledWith(3);
    expect(fetch).toHaveBeenCalledTimes(3);
    stop();
  });

  it('surfaces an error and settles only on the INITIAL load; background failures are swallowed', async () => {
    let call = 0;
    const fetch = vi.fn(() => {
      call += 1;
      return call === 1 ? Promise.resolve('ok') : Promise.reject(new Error('boom'));
    });
    const onData = vi.fn();
    const onError = vi.fn();
    const onSettled = vi.fn();
    const stop = startPoll({ fetch, onData, onError, onSettled, intervalMs: 1000 });

    await vi.advanceTimersByTimeAsync(0);
    expect(onData).toHaveBeenCalledWith('ok');
    expect(onSettled).toHaveBeenCalledTimes(1);
    expect(onError).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1000); // a background failure
    expect(onError).not.toHaveBeenCalled(); // not surfaced — last good view is kept
    expect(onSettled).toHaveBeenCalledTimes(1); // only the initial load settles
    stop();
  });

  it('tells a background failure apart, keeping the last good view, and only after the first load', async () => {
    const results: Array<() => Promise<string>> = [
      () => Promise.reject(new Error('down')),
      () => Promise.resolve('ok'),
      () => Promise.reject(new Error('flaky')),
      () => Promise.resolve('back'),
    ];
    const fetch = vi.fn(() => results.shift()!());
    const onData = vi.fn();
    const onError = vi.fn();
    const onRefreshError = vi.fn();
    const stop = startPoll({ fetch, onData, onError, onRefreshError, intervalMs: 1000 });

    await vi.advanceTimersByTimeAsync(0);
    expect(onError).toHaveBeenCalledWith('down', expect.any(Error));
    expect(onRefreshError).not.toHaveBeenCalled(); // the initial load's failure is onError's
    await vi.advanceTimersByTimeAsync(1000);
    expect(onData).toHaveBeenLastCalledWith('ok');
    await vi.advanceTimersByTimeAsync(1000);
    expect(onRefreshError).toHaveBeenCalledWith('flaky');
    expect(onData).toHaveBeenCalledTimes(1); // the last good view stands
    await vi.advanceTimersByTimeAsync(1000);
    expect(onData).toHaveBeenLastCalledWith('back');
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onRefreshError).toHaveBeenCalledTimes(1);
    stop();
  });

  it('surfaces an initial-load failure', async () => {
    const fetch = vi.fn(() => Promise.reject(new Error('down')));
    const onError = vi.fn();
    const onSettled = vi.fn();
    const stop = startPoll({ fetch, onData: vi.fn(), onError, onSettled, intervalMs: 1000 });

    await vi.advanceTimersByTimeAsync(0);
    // The message, without an "Error: " prefix, and the error itself (for its status).
    expect(onError).toHaveBeenCalledWith('down', expect.objectContaining({ message: 'down' }));
    expect(onSettled).toHaveBeenCalledTimes(1);
    stop();
  });

  it('stops polling once `until` is satisfied', async () => {
    const states = ['running', 'running', 'succeeded'];
    const fetch = vi.fn(() => Promise.resolve(states.shift() ?? 'succeeded'));
    const stop = startPoll({
      fetch,
      onData: vi.fn(),
      until: (s) => s === 'succeeded',
      intervalMs: 1000,
    });

    await vi.advanceTimersByTimeAsync(0); // running
    await vi.advanceTimersByTimeAsync(1000); // running
    await vi.advanceTimersByTimeAsync(1000); // succeeded → stop
    expect(fetch).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(5000); // no further ticks
    expect(fetch).toHaveBeenCalledTimes(3);
    stop();
  });

  it('never ticks when the INITIAL load already satisfies `until` (e.g. a finished run)', async () => {
    const fetch = vi.fn(() => Promise.resolve('succeeded'));
    const onData = vi.fn();
    const stop = startPoll({ fetch, onData, until: (s) => s === 'succeeded', intervalMs: 1000 });

    await vi.advanceTimersByTimeAsync(5000);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(onData).toHaveBeenCalledTimes(1);
    stop();
  });

  it('skips ticks while a fetch is still in flight, so slow fetches never overlap', async () => {
    // A fetch slower than intervalMs must not let subsequent ticks pile up concurrent requests
    // (which also removes the out-of-order-response race): the interval is skipped until the
    // pending fetch resolves.
    const pending: Array<(v: number) => void> = [];
    const fetch = vi.fn(() => new Promise<number>((resolve) => pending.push(resolve)));
    const onData = vi.fn();
    const stop = startPoll({ fetch, onData, intervalMs: 1000 });

    expect(fetch).toHaveBeenCalledTimes(1); // immediate load, still awaiting
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetch).toHaveBeenCalledTimes(1); // tick skipped — previous fetch in flight
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetch).toHaveBeenCalledTimes(1);

    pending[0](1); // first fetch resolves
    await vi.advanceTimersByTimeAsync(0);
    expect(onData).toHaveBeenLastCalledWith(1);

    await vi.advanceTimersByTimeAsync(1000); // now free to fetch again
    expect(fetch).toHaveBeenCalledTimes(2);
    pending[1](2);
    await vi.advanceTimersByTimeAsync(0);
    expect(onData).toHaveBeenLastCalledWith(2);
    stop();
  });

  it('teardown cancels the interval and ignores an in-flight result', async () => {
    const d = deferred<number>();
    const fetch = vi.fn(() => d.promise);
    const onData = vi.fn();
    const stop = startPoll({ fetch, onData, intervalMs: 1000 });

    stop(); // tear down before the in-flight fetch resolves
    d.resolve(42);
    await vi.advanceTimersByTimeAsync(0);
    expect(onData).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(5000);
    expect(fetch).toHaveBeenCalledTimes(1); // no further ticks after teardown
  });

  it('refreshes at once on a nudge, after a pending load when one is in flight', async () => {
    const pending: Array<(v: number) => void> = [];
    const fetch = vi.fn(() => new Promise<number>((resolve) => pending.push(resolve)));
    const onData = vi.fn();
    const nudge = new PollNudge();
    const stop = startPoll({ fetch, onData, intervalMs: 10_000, nudge });

    // Nudged while the first load is pending: no overlap, but a load once it lands.
    nudge.now();
    expect(fetch).toHaveBeenCalledTimes(1);
    pending[0](1);
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).toHaveBeenCalledTimes(2);
    pending[1](2);
    await vi.advanceTimersByTimeAsync(0);
    expect(onData).toHaveBeenLastCalledWith(2);
    // Idle: a nudge loads now, not at the next tick.
    nudge.now();
    expect(fetch).toHaveBeenCalledTimes(3);
    pending[2](3);
    await vi.advanceTimersByTimeAsync(0);
    expect(onData).toHaveBeenLastCalledWith(3);
    // Once stopped, a nudge does nothing.
    stop();
    nudge.now();
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  describe('while the tab is hidden', () => {
    // A stand-in `document` whose visibility a test flips (the tests run under node, which has none).
    function fakeDocument() {
      const listeners = new Set<() => void>();
      const doc = {
        visibilityState: 'visible' as DocumentVisibilityState,
        addEventListener: (_: string, fn: () => void) => listeners.add(fn),
        removeEventListener: (_: string, fn: () => void) => listeners.delete(fn),
        show(visible: boolean) {
          doc.visibilityState = visible ? 'visible' : 'hidden';
          for (const fn of listeners) fn();
        },
        listeners,
      };
      vi.stubGlobal('document', doc);
      return doc;
    }
    afterEach(() => vi.unstubAllGlobals());

    it('skips its ticks, and refreshes at once when the tab is shown again', async () => {
      const doc = fakeDocument();
      const fetch = vi.fn(() => Promise.resolve(1));
      const stop = startPoll({ fetch, onData: () => {}, intervalMs: 1000 });
      await vi.advanceTimersByTimeAsync(0);
      expect(fetch).toHaveBeenCalledTimes(1);

      doc.show(false);
      await vi.advanceTimersByTimeAsync(5000);
      expect(fetch).toHaveBeenCalledTimes(1);

      doc.show(true);
      await vi.advanceTimersByTimeAsync(0);
      expect(fetch).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(1000);
      expect(fetch).toHaveBeenCalledTimes(3);
      stop();
      expect(doc.listeners.size).toBe(0);
    });

    it('does not refresh on being shown once stopped, or while a load is pending', async () => {
      const doc = fakeDocument();
      const pending = deferred<number>();
      const fetch = vi.fn(() => pending.promise);
      const stop = startPoll({ fetch, onData: () => {}, intervalMs: 1000 });
      doc.show(false);
      doc.show(true);
      expect(fetch).toHaveBeenCalledTimes(1); // the initial load is still in flight
      pending.resolve(1);
      await vi.advanceTimersByTimeAsync(0);
      stop();
      doc.show(false);
      doc.show(true);
      expect(fetch).toHaveBeenCalledTimes(1);
    });

    it('stops listening once `until` ends the loop', async () => {
      const doc = fakeDocument();
      const fetch = vi.fn(() => Promise.resolve('done'));
      startPoll({ fetch, onData: () => {}, until: () => true, intervalMs: 1000 });
      await vi.advanceTimersByTimeAsync(0);
      expect(doc.listeners.size).toBe(0);
      doc.show(false);
      doc.show(true);
      expect(fetch).toHaveBeenCalledTimes(1);
    });
  });
});
