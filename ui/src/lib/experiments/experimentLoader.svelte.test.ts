import { afterEach, describe, expect, it, vi } from 'vitest';

// The detail is read through `getExperiment`; each test answers it by hand.
const api = vi.hoisted(() => ({ getExperiment: vi.fn() }));
vi.mock('$lib/api', () => api);

const { ExperimentLoader } = await import('./experimentLoader.svelte');

const detail = (name: string) =>
  ({
    experiment: { id: 'e1', name, project: 'p' },
    runs: [{ run_id: 'r1', terminal: false }],
  }) as never;

afterEach(() => vi.useRealTimers());

describe('ExperimentLoader', () => {
  it('keeps a saved edit over a poll that was already on its way', async () => {
    vi.useFakeTimers();
    api.getExperiment.mockResolvedValueOnce(detail('Old'));
    const loader = new ExperimentLoader();
    const stop = loader.watch('e1');
    await vi.advanceTimersByTimeAsync(0);
    expect(loader.detail?.experiment.name).toBe('Old');
    // A poll goes out, and the rename is saved before it answers with what it read before.
    let answer!: (d: unknown) => void;
    api.getExperiment.mockReturnValueOnce(new Promise((r) => (answer = r)));
    api.getExperiment.mockResolvedValue(detail('New'));
    await vi.advanceTimersByTimeAsync(15_000);
    loader.updated({ id: 'e1', name: 'New', project: 'p' } as never);
    answer(detail('Old'));
    await vi.advanceTimersByTimeAsync(0);
    expect(loader.detail?.experiment.name).toBe('New');
    stop();
  });
});
