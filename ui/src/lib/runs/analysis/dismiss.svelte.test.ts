import { beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({ dismissAttention: vi.fn() }));
vi.mock('#lib/api/index.js', () => api);

import { Dismissals, dismissAllLabel, withoutDismissed } from './dismiss.svelte';

const run = (run_id: string) => ({ run_id });

describe('withoutDismissed', () => {
  it('drops the hidden runs and lowers the total by as many as it took out', () => {
    const row = { runs: [run('a'), run('b'), run('c')], total: 7 };
    expect(withoutDismissed(row, new Set(['b', 'zz']))).toEqual({
      runs: [run('a'), run('c')],
      total: 6,
    });
  });

  it('takes nothing twice: a run the server no longer lists lowers nothing', () => {
    const row = { runs: [run('a')], total: 1 };
    expect(withoutDismissed(row, new Set(['b']))).toEqual(row);
    expect(withoutDismissed(row, new Set())).toBe(row);
  });
});

describe('dismissAllLabel', () => {
  it('says all when the rows are every one, else these', () => {
    expect(dismissAllLabel(3, 3)).toBe('Dismiss all 3');
    expect(dismissAllLabel(50, 72)).toBe('Dismiss these 50');
  });
});

describe('Dismissals', () => {
  beforeEach(() => api.dismissAttention.mockReset());

  it('hides at once, sends exactly the runs named, and keeps them hidden once taken', async () => {
    let answer!: (v: unknown) => void;
    api.dismissAttention.mockReturnValue(new Promise((res) => (answer = res)));
    const d = new Dismissals();
    const taken = d.dismiss(['a', 'b'], '2 analyses');
    expect([...d.hidden]).toEqual(['a', 'b']);
    expect(api.dismissAttention).toHaveBeenCalledWith(['a', 'b']);
    expect(d.announcement).toBe('Dismissed 2 analyses.');
    answer({ attention: 0 });
    expect(await taken).toBe(true);
    expect([...d.hidden]).toEqual(['a', 'b']);
    expect(d.error).toBeNull();
    // Already dismissed: nothing is sent again.
    expect(await d.dismiss(['a'], 'batch-1')).toBe(true);
    expect(api.dismissAttention).toHaveBeenCalledTimes(1);
  });

  it('shows the runs again and says why when the server refuses', async () => {
    api.dismissAttention.mockResolvedValueOnce({ attention: 1 });
    const d = new Dismissals();
    await d.dismiss(['a'], 'batch-1');
    api.dismissAttention.mockRejectedValueOnce(new Error('503 store unavailable'));
    expect(await d.dismiss(['b', 'a'], '2 analyses')).toBe(false);
    // Only the refused run comes back; the one taken earlier stays hidden.
    expect(api.dismissAttention).toHaveBeenLastCalledWith(['b']);
    expect([...d.hidden]).toEqual(['a']);
    expect(d.error).toBe('503 store unavailable');
    expect(d.announcement).toBe("Couldn't dismiss 2 analyses.");
  });

  it('tells the caller how many still need attention once the server has it', async () => {
    api.dismissAttention.mockResolvedValueOnce({ attention: 4 });
    const d = new Dismissals();
    const heard: (number | null)[] = [];
    expect(await d.dismiss(['a'], 'batch-1', (n) => heard.push(n))).toBe(true);
    expect(heard).toEqual([4]);
    // Taken but not counted (the count read failed): taken all the same, the count unknown.
    api.dismissAttention.mockResolvedValueOnce({});
    expect(await d.dismiss(['b'], 'batch-2', (n) => heard.push(n))).toBe(true);
    expect(heard).toEqual([4, null]);
    // A refusal is never reported as taken.
    api.dismissAttention.mockRejectedValueOnce(new Error('boom'));
    expect(await d.dismiss(['c'], 'batch-3', (n) => heard.push(n))).toBe(false);
    expect(heard).toEqual([4, null]);
  });

  it('reports a run still on its way as dismissed only once that dismissal is confirmed', async () => {
    let refuse!: (e: Error) => void;
    api.dismissAttention.mockReturnValueOnce(new Promise((_, rej) => (refuse = rej)));
    const d = new Dismissals();
    const first = d.dismiss(['a'], 'batch-1');
    // Asked again while the first is in flight: nothing new is sent, and the answer waits on it.
    const again = d.dismiss(['a'], 'batch-1');
    expect(api.dismissAttention).toHaveBeenCalledTimes(1);
    refuse(new Error('503'));
    expect(await first).toBe(false);
    expect(await again).toBe(false);
    expect([...d.hidden]).toEqual([]);
  });
});
