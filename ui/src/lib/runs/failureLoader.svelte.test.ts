import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { FailureSummary } from '#lib/api/index.js';

// Each request is recorded; `answers` say what the next ones answer (the last one repeats), and a
// string answer is a failed request.
let answers: (Partial<FailureSummary> | string)[] = [];
const calls: string[] = [];
let deferred: ((s: FailureSummary) => void) | null = null;
let hold = false;

const summary = (step: string, pending = false): Partial<FailureSummary> => ({
  failed_steps: 0,
  items: [{ step } as FailureSummary['items'][number]],
  ...(pending ? { pending: true } : {}),
});

vi.mock('#lib/api/index.js', () => ({
  getFailureSummary: async (runId: string): Promise<FailureSummary> => {
    calls.push(runId);
    const answer = answers.length > 1 ? answers.shift()! : answers[0];
    if (hold) return new Promise((resolve) => (deferred = resolve));
    if (typeof answer === 'string') throw new Error(answer);
    return answer as FailureSummary;
  },
}));

const { FailureSummaryLoader, SETTLE_MS, SETTLE_TRIES } = await import('./failureLoader.svelte');

/** Let settled promises run their handlers. */
const flush = async () => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

beforeEach(() => {
  vi.useFakeTimers();
  answers = [summary('done')];
  calls.length = 0;
  deferred = null;
  hold = false;
});
afterEach(() => vi.useRealTimers());

describe('FailureSummaryLoader', () => {
  it('asks once for a summary that is not pending', async () => {
    const why = new FailureSummaryLoader();
    const stop = why.watch('r', true);
    await flush();
    expect(why.summary?.items[0].step).toBe('done');
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toEqual(['r']);
    stop?.();
  });

  it('asks nothing for a run that did not fail', async () => {
    const why = new FailureSummaryLoader();
    expect(why.watch('r', false)).toBeUndefined();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toEqual([]);
  });

  it('asks again while the summary is pending, and stops once it is settled', async () => {
    answers = [summary('early', true), summary('early', true), summary('settled')];
    const why = new FailureSummaryLoader();
    const stop = why.watch('r', true);
    await flush();
    expect(why.summary?.items[0].step).toBe('early');
    await vi.advanceTimersByTimeAsync(SETTLE_MS);
    expect(calls).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(SETTLE_MS);
    expect(why.summary?.items[0].step).toBe('settled');
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(3);
    stop?.();
  });

  it('stops asking after SETTLE_TRIES re-asks of a summary that stays pending', async () => {
    answers = [summary('early', true)];
    const why = new FailureSummaryLoader();
    const stop = why.watch('r', true);
    await flush();
    await vi.advanceTimersByTimeAsync(SETTLE_MS * (SETTLE_TRIES + 10));
    expect(calls).toHaveLength(1 + SETTLE_TRIES);
    expect(why.summary?.items[0].step).toBe('early');
    stop?.();
  });

  it('re-asks for as long as the server may say pending', () => {
    // The API doc of `pending` names the window (`FAILURE_SUMMARY_SETTLE_SECS`; a backend test
    // pins the sentence to the constant).
    const doc = JSON.parse(
      readFileSync(fileURLToPath(new URL('../generated/openapi.json', import.meta.url)), 'utf8'),
    );
    const text: string = doc.components.schemas.FailureSummary.properties.pending.description;
    const seconds = Number(/at most (\d+) seconds/.exec(text)?.[1]);
    expect(seconds).toBeGreaterThan(0);
    expect(SETTLE_MS * SETTLE_TRIES).toBeGreaterThanOrEqual(seconds * 1000);
  });

  it('a failed first ask says why, and asks nothing more', async () => {
    answers = ['down'];
    const why = new FailureSummaryLoader();
    const stop = why.watch('r', true);
    await flush();
    expect(why.error).toContain('down');
    expect(why.summary).toBeNull();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toEqual(['r']);
    stop?.();
  });

  it('cancel stops the re-asks and drops a late answer', async () => {
    answers = [summary('early', true), summary('late')];
    const why = new FailureSummaryLoader();
    const stop = why.watch('r', true);
    await flush();
    hold = true;
    await vi.advanceTimersByTimeAsync(SETTLE_MS);
    expect(calls).toHaveLength(2);
    stop?.();
    deferred!(summary('late') as FailureSummary);
    await flush();
    expect(why.summary?.items[0].step).toBe('early');
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(2);
  });

  it('a failed re-ask keeps the earlier summary and stops', async () => {
    answers = [summary('early', true), 'down'];
    const why = new FailureSummaryLoader();
    const stop = why.watch('r', true);
    await flush();
    await vi.advanceTimersByTimeAsync(SETTLE_MS);
    expect(why.summary?.items[0].step).toBe('early');
    expect(why.error).toBeNull();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(2);
    stop?.();
  });
});
