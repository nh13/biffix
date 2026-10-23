import { beforeEach, describe, expect, it, vi } from 'vitest';
import { storageWith } from '$lib/api/testing';

// The view store talks to `/me/prefs` through these two calls; each test scripts their answers.
const api = vi.hoisted(() => ({
  getUserPrefs: vi.fn(),
  putUserPrefs: vi.fn(),
}));
vi.mock('$lib/api', () => api);

/** A promise whose settling the test controls, to reorder replies. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Let the store's queued saves go out. */
async function settle() {
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

async function freshStore() {
  vi.resetModules();
  return import('./userPrefs.svelte');
}

beforeEach(() => {
  api.getUserPrefs.mockReset();
  api.putUserPrefs.mockReset();
});

describe('pins', () => {
  it('refuses to change pins before the saved ones load', async () => {
    const { togglePin, view } = await freshStore();
    await togglePin('a');
    expect(api.putUserPrefs).not.toHaveBeenCalled();
    expect(view.pinned).toEqual([]);
  });

  it('sends one operation per click, never the whole list', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: ['x'] });
    const { loadPins, togglePin, view } = await freshStore();
    await loadPins();
    api.putUserPrefs.mockResolvedValue({ pinned_workflows: ['x', 'a'] });
    await togglePin('a');
    expect(api.putUserPrefs).toHaveBeenLastCalledWith({ pin: 'a' });
    api.putUserPrefs.mockResolvedValue({ pinned_workflows: ['a'] });
    await togglePin('x');
    expect(api.putUserPrefs).toHaveBeenLastCalledWith({ unpin: 'x' });
    expect(view.pinned).toEqual(['a']);
  });

  it('applies only the newest reply when replies arrive out of order', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [] });
    const { loadPins, togglePin, view } = await freshStore();
    await loadPins();
    const first = deferred<{ pinned_workflows: string[] }>();
    const second = deferred<{ pinned_workflows: string[] }>();
    api.putUserPrefs.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const a = togglePin('a');
    const b = togglePin('b');
    second.resolve({ pinned_workflows: ['a', 'b'] });
    await b;
    first.resolve({ pinned_workflows: ['a'] });
    await a;
    expect(view.pinned).toEqual(['a', 'b']);
  });

  it('undoes a refused pin and says why', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: ['x'] });
    const { loadPins, togglePin, view } = await freshStore();
    await loadPins();
    api.putUserPrefs.mockRejectedValue(new Error('at most 50 pinned pipelines'));
    await togglePin('a');
    expect(view.pinned).toEqual(['x']);
    expect(view.pinError).toBe('at most 50 pinned pipelines');
  });

  it('keeps a pin made while the saved pins were still loading', async () => {
    const load = deferred<{ pinned_workflows: string[] }>();
    api.getUserPrefs.mockResolvedValueOnce({ pinned_workflows: ['x'] });
    const { loadPins, loadView, togglePin, view } = await freshStore();
    await loadPins();
    api.getUserPrefs.mockReturnValueOnce(load.promise);
    const pending = loadView();
    api.putUserPrefs.mockResolvedValue({ pinned_workflows: ['x', 'a'] });
    await togglePin('a');
    load.resolve({ pinned_workflows: ['x'] });
    await pending;
    expect(view.pinned).toEqual(['x', 'a']);
  });
});

describe('the first-visit tour', () => {
  it('is unknown until the preferences load, then follows `tour_done_at`', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [] });
    const { loadView, view } = await freshStore();
    expect(view.tourDone).toBeNull();
    await loadView();
    expect(view.tourDone).toBe(false);

    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [], tour_done_at: 1 });
    const again = await freshStore();
    await again.loadView();
    expect(again.view.tourDone).toBe(true);
  });

  it('stays unknown (so never shown) when the preferences cannot load', async () => {
    api.getUserPrefs.mockRejectedValue(new Error('down'));
    const { loadView, view } = await freshStore();
    await loadView();
    expect(view.tourDone).toBeNull();
  });

  it('records a finish, and ends it for the page even when the save is refused', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [] });
    const { finishTour, loadView, view } = await freshStore();
    await loadView();
    api.putUserPrefs.mockRejectedValue(new Error('refused'));
    await finishTour();
    expect(api.putUserPrefs).toHaveBeenLastCalledWith({ tour_done: true });
    expect(view.tourDone).toBe(true);
  });

  it('replays: not done again, and recorded as such', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [], tour_done_at: 1 });
    const { loadView, replayTour, view } = await freshStore();
    await loadView();
    api.putUserPrefs.mockResolvedValue({ pinned_workflows: [] });
    await replayTour();
    expect(api.putUserPrefs).toHaveBeenLastCalledWith({ tour_done: false });
    expect(view.tourDone).toBe(false);
  });

  it("keeps this page's finish over a slower load", async () => {
    const slow = deferred<{ pinned_workflows: string[] }>();
    api.getUserPrefs.mockReturnValue(slow.promise);
    api.putUserPrefs.mockResolvedValue({ pinned_workflows: [] });
    const { finishTour, loadView, view } = await freshStore();
    const loading = loadView();
    await finishTour();
    slow.resolve({ pinned_workflows: [] });
    await loading;
    expect(view.tourDone).toBe(true);
  });

  it('sends tour writes one at a time, ending with the latest', async () => {
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [], tour_done_at: 1 });
    const { finishTour, loadView, replayTour, view } = await freshStore();
    await loadView();
    const replayed = deferred<{ pinned_workflows: string[] }>();
    api.putUserPrefs.mockReturnValueOnce(replayed.promise).mockResolvedValue({});
    const replay = replayTour();
    await Promise.resolve();
    expect(api.putUserPrefs).toHaveBeenCalledTimes(1);
    // A skip while the replay is still in flight waits for it, then is sent.
    const skip = finishTour();
    // And a replay and skip queued behind it are dropped for the newest.
    void replayTour();
    const last = finishTour();
    expect(api.putUserPrefs).toHaveBeenCalledTimes(1);
    replayed.resolve({ pinned_workflows: [] });
    await Promise.all([replay, skip, last]);
    expect(api.putUserPrefs.mock.calls).toEqual([[{ tour_done: false }], [{ tour_done: true }]]);
    expect(view.tourDone).toBe(true);
  });

  it("adopts the tour's answer from a later pins load when the first load failed", async () => {
    api.getUserPrefs.mockRejectedValueOnce(new Error('down'));
    const { loadPins, loadView, view } = await freshStore();
    await loadView();
    expect(view.tourDone).toBeNull();
    api.getUserPrefs.mockResolvedValue({ pinned_workflows: [] });
    await loadPins();
    expect(view.tourDone).toBe(false);
  });
});

describe('the view', () => {
  // The store reads and writes this browser's storage; an in-memory one stands in for it, kept
  // across a reload.
  beforeEach(() => {
    vi.stubGlobal('localStorage', storageWith({}));
  });

  it('keeps a switch whose save had no reply before the page was left, and sends it again', async () => {
    api.putUserPrefs.mockReturnValueOnce(new Promise(() => {}));
    const first = await freshStore();
    void first.setView('expert');

    // A reload: the server still holds the old view.
    api.getUserPrefs.mockResolvedValue({ view: 'lab' });
    api.putUserPrefs.mockResolvedValue({});
    const { loadView, view } = await freshStore();
    await loadView();
    expect(view.mode).toBe('expert');
    expect(api.putUserPrefs).toHaveBeenLastCalledWith({ view: 'expert' });
  });

  it('adopts the server view once every switch was stored', async () => {
    api.putUserPrefs.mockResolvedValue({});
    const first = await freshStore();
    await first.setView('expert');

    api.getUserPrefs.mockResolvedValue({ view: 'lab' });
    const { loadView, view } = await freshStore();
    await loadView();
    expect(view.mode).toBe('lab');
  });

  it('sends switches one at a time, the latest last, and keeps a newer one’s refusal', async () => {
    const older = deferred<object>();
    const newer = deferred<object>();
    api.putUserPrefs.mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
    const { setView, view } = await freshStore();
    void setView('expert');
    await settle();
    void setView('lab');
    const last = setView('expert');
    // The first is on its way; the others wait for it, and the one overtaken is never sent.
    await settle();
    expect(api.putUserPrefs.mock.calls).toEqual([[{ view: 'expert' }]]);
    newer.reject(new Error('503 busy'));
    older.resolve({});
    await last;
    expect(api.putUserPrefs.mock.calls).toEqual([[{ view: 'expert' }], [{ view: 'expert' }]]);
    // The latest choice's save failed: said, though the older one was stored.
    expect(view.saveError).toBe('503 busy');
  });

  it('stays unsynced while a newer switch is still on its way', async () => {
    const older = deferred<object>();
    api.putUserPrefs.mockReturnValueOnce(older.promise).mockReturnValueOnce(new Promise(() => {}));
    const first = await freshStore();
    const saved = first.setView('expert');
    await settle();
    void first.setView('lab');
    older.resolve({});
    await saved;

    api.getUserPrefs.mockResolvedValue({ view: 'expert' });
    api.putUserPrefs.mockResolvedValue({});
    const { loadView, view } = await freshStore();
    await loadView();
    expect(view.mode).toBe('lab');
  });
});
