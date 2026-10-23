import { describe, expect, it } from 'vitest';
import { fetchWindow, MAX_TASK_PAGE, newestOnly, uniqueBy } from './taskWindow';

const task = (task_key: string) => ({ task_key });

describe('uniqueBy', () => {
  it('keeps the first of each key, in order', () => {
    const items = [task('a'), task('b'), task('a'), task('c'), task('b')];
    expect(uniqueBy(items, (t) => t.task_key).map((t) => t.task_key)).toEqual(['a', 'b', 'c']);
  });
});

describe('newestOnly', () => {
  function deferred<T>() {
    let resolve!: (v: T) => void;
    let reject!: (e: unknown) => void;
    const promise = new Promise<T>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    return { promise, resolve, reject };
  }

  it('drops an older call that settles after a newer one started', async () => {
    const calls = [deferred<string>(), deferred<string>()];
    let n = 0;
    const fetch = newestOnly(() => calls[n++].promise);
    const older = fetch();
    const newer = fetch();
    calls[1].resolve('new');
    calls[0].resolve('old');
    expect(await newer).toBe('new');
    expect(await older).toBeNull();
  });

  it("swallows a superseded call's failure, and reports the newest one's", async () => {
    const calls = [deferred<string>(), deferred<string>()];
    let n = 0;
    const fetch = newestOnly(() => calls[n++].promise);
    const older = fetch();
    const newer = fetch();
    calls[0].reject(new Error('stale'));
    calls[1].reject(new Error('fresh'));
    expect(await older).toBeNull();
    await expect(newer).rejects.toThrow('fresh');
  });
});

describe('fetchWindow', () => {
  it('fetches a small window in one request', async () => {
    const asked: [number, number][] = [];
    const page = await fetchWindow(async (offset, limit) => {
      asked.push([offset, limit]);
      return { tasks: [task('a'), task('b')], total: 2 };
    }, 200);
    expect(asked).toEqual([[0, 200]]);
    expect(page.tasks.map((t) => t.task_key)).toEqual(['a', 'b']);
  });

  it('fetches a window past the server page size in chunks, without repeating a row', async () => {
    const asked: [number, number][] = [];
    // A task inserted between the chunks' reads shifts the second chunk by one: its first row is
    // the first chunk's last.
    const page = await fetchWindow(async (offset, limit) => {
      asked.push([offset, limit]);
      const start = offset === 0 ? 0 : offset - 1;
      return {
        tasks: Array.from({ length: limit }, (_, i) => task(`t${start + i}`)),
        total: 3000 + offset,
      };
    }, MAX_TASK_PAGE + 200);
    expect(asked).toEqual([
      [0, MAX_TASK_PAGE],
      [MAX_TASK_PAGE, 200],
    ]);
    const keys = page.tasks.map((t) => t.task_key);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys.length).toBe(MAX_TASK_PAGE + 199);
    expect(page.total).toBe(3000 + MAX_TASK_PAGE);
  });
});
