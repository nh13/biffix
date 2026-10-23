import { beforeEach, describe, expect, it, vi } from 'vitest';

// The store talks to the API through these calls, and joins the status poll's beat through these.
const api = vi.hoisted(() => ({
  listNotifications: vi.fn(),
  markNotificationsRead: vi.fn(),
}));
vi.mock('$lib/api', () => api);
const live = vi.hoisted(() => ({
  beats: new Set<() => void>(),
  stops: 0,
}));
vi.mock('$lib/status/live.svelte', () => ({
  watchStatus: () => () => void live.stops++,
  onStatusProbe: (fn: () => void) => {
    live.beats.add(fn);
    return () => live.beats.delete(fn);
  },
}));

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

const note = (id: string, read = false, kind = 'failed') => ({
  id,
  run_id: `run_${id}`,
  kind,
  at: 1,
  read,
});
const LIST = { items: [note('a'), note('b', true)], unread: 1, unread_failed: 1 };

async function freshStore() {
  vi.resetModules();
  return import('./notifications.svelte');
}

describe('notifications store', () => {
  beforeEach(() => {
    api.listNotifications.mockReset();
    api.markNotificationsRead.mockReset();
    live.beats.clear();
    live.stops = 0;
  });

  it('loads once watched, refreshes on the status beat, and leaves it when released', async () => {
    api.listNotifications.mockResolvedValue(LIST);
    const { notifications, watchNotifications } = await freshStore();
    const first = watchNotifications();
    const second = watchNotifications();
    await vi.waitFor(() => expect(notifications.list).toEqual(LIST));
    expect(api.listNotifications).toHaveBeenCalledWith(20);
    expect(live.beats.size).toBe(1);
    for (const beat of live.beats) beat();
    await vi.waitFor(() => expect(api.listNotifications).toHaveBeenCalledTimes(2));
    first();
    first(); // a second release of the same watcher is a no-op
    expect(live.beats.size).toBe(1);
    second();
    expect([live.beats.size, live.stops]).toEqual([0, 1]);
  });

  it('keeps the last good list when a refresh fails, and says why', async () => {
    api.listNotifications.mockResolvedValueOnce(LIST).mockRejectedValueOnce(new Error('503 down'));
    const { notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    await refreshNotifications();
    expect(notifications.list).toEqual(LIST);
    expect(notifications.error).toBe('503 down');
    api.listNotifications.mockResolvedValueOnce(LIST);
    await refreshNotifications();
    expect(notifications.error).toBeNull();
  });

  it('marks read at once, then takes the server’s counts', async () => {
    const after = deferred<typeof LIST>();
    api.listNotifications.mockResolvedValueOnce(LIST).mockReturnValueOnce(after.promise);
    const reply = deferred<{ unread: number; unread_failed: number }>();
    api.markNotificationsRead.mockReturnValue(reply.promise);
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    const done = markRead({ ids: ['a'] });
    expect(notifications.list!.items[0].read).toBe(true);
    expect(notifications.list!.unread).toBe(0);
    // The server knows of more unread than the bell lists.
    reply.resolve({ unread: 4, unread_failed: 2 });
    await vi.waitFor(() =>
      expect([notifications.list!.unread, notifications.list!.unread_failed]).toEqual([4, 2]),
    );
    expect(api.markNotificationsRead).toHaveBeenCalledWith({ ids: ['a'] });
    after.resolve(LIST);
    expect(await done).toBe(true);
  });

  it('drops a refresh that a read overtook, then asks again', async () => {
    const stale = deferred<typeof LIST>();
    const readAll = {
      items: LIST.items.map((n) => ({ ...n, read: true })),
      unread: 0,
      unread_failed: 0,
    };
    api.listNotifications
      .mockResolvedValueOnce(LIST)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValue(readAll);
    api.markNotificationsRead.mockResolvedValue({ unread: 0, unread_failed: 0 });
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    const refreshing = refreshNotifications();
    const reading = markRead({ all: true });
    stale.resolve(LIST);
    await Promise.all([refreshing, reading]);
    expect(notifications.list).toEqual(readAll);
    expect(api.listNotifications).toHaveBeenCalledTimes(3);
  });

  it('drops a refresh that started during a read and answered first', async () => {
    const reply = deferred<{ unread: number; unread_failed: number }>();
    const server = { items: [note('a', true), note('b', true)], unread: 0, unread_failed: 0 };
    api.listNotifications.mockResolvedValueOnce(LIST).mockResolvedValue(server);
    api.markNotificationsRead.mockReturnValue(reply.promise);
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    const reading = markRead({ ids: ['a'] });
    // The beat during the read: the server's answer (before it took the read) is not shown.
    api.listNotifications.mockResolvedValueOnce(LIST);
    await refreshNotifications();
    expect(notifications.list!.items[0].read).toBe(true);
    expect(notifications.list!.unread).toBe(0);
    reply.resolve({ unread: 0, unread_failed: 0 });
    expect(await reading).toBe(true);
    // Once the read settled, a fresh list was asked for and taken.
    expect(api.listNotifications).toHaveBeenCalledTimes(3);
    expect(notifications.list).toEqual(server);
  });

  it('takes only the newest read’s counts when two answer out of order', async () => {
    const first = deferred<{ unread: number; unread_failed: number }>();
    const second = deferred<{ unread: number; unread_failed: number }>();
    const fresh = deferred<typeof LIST>();
    const LIST3 = { items: [note('a'), note('b'), note('c')], unread: 3, unread_failed: 3 };
    api.listNotifications.mockResolvedValueOnce(LIST3).mockReturnValueOnce(fresh.promise);
    api.markNotificationsRead
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    const a = markRead({ ids: ['a'] });
    const b = markRead({ ids: ['b'] });
    expect(notifications.list!.unread).toBe(1);
    second.resolve({ unread: 1, unread_failed: 1 });
    await vi.waitFor(() => expect(notifications.list!.unread).toBe(1));
    // The older read answers last, with counts from before the newer one: not taken.
    first.resolve({ unread: 2, unread_failed: 2 });
    await vi.waitFor(() => expect(api.listNotifications).toHaveBeenCalledTimes(2));
    expect(notifications.list!.unread).toBe(1);
    const server = {
      items: [note('a', true), note('b', true), note('c')],
      unread: 1,
      unread_failed: 1,
    };
    fresh.resolve(server);
    await Promise.all([a, b]);
    expect(notifications.list).toEqual(server);
  });

  it('corrects a refused read even while a refresh is in flight', async () => {
    const stale = deferred<typeof LIST>();
    api.listNotifications
      .mockResolvedValueOnce(LIST)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValue(LIST);
    api.markNotificationsRead.mockRejectedValue(new Error('500 boom'));
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    const refreshing = refreshNotifications();
    const reading = markRead({ ids: ['a'] });
    expect(notifications.list!.items[0].read).toBe(true);
    await vi.waitFor(() => expect(notifications.readError).toBe('500 boom'));
    stale.resolve(LIST);
    expect(await reading).toBe(false);
    await refreshing;
    // No beat needed: the list was asked for again after the refusal, and taken.
    expect(api.listNotifications).toHaveBeenCalledTimes(3);
    expect(notifications.list!.items[0].read).toBe(false);
    expect(notifications.error).toBeNull();
  });

  it('asks again when the server refuses a read', async () => {
    api.listNotifications.mockResolvedValue(LIST);
    api.markNotificationsRead.mockRejectedValue(new Error('500 boom'));
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    expect(await markRead({ ids: ['a'] })).toBe(false);
    // The fresh list (still unread) replaces the optimistic one.
    expect(notifications.list!.items[0].read).toBe(false);
    expect(api.listNotifications).toHaveBeenCalledTimes(2);
    // The refusal is a read's, kept past the refresh that followed (which succeeded).
    expect(notifications.readError).toBe('500 boom');
    expect(notifications.error).toBeNull();
    api.markNotificationsRead.mockResolvedValue({ unread: 0, unread_failed: 0 });
    await markRead({ ids: ['a'] });
    expect(notifications.readError).toBeNull();
  });

  it('loads at once after a read that overtook the first load', async () => {
    const firstLoad = deferred<typeof LIST>();
    const server = { items: [note('a', true), note('b', true)], unread: 0, unread_failed: 0 };
    api.listNotifications.mockReturnValueOnce(firstLoad.promise).mockResolvedValue(server);
    api.markNotificationsRead.mockResolvedValue({ unread: 0, unread_failed: 0 });
    const { markRead, notifications, watchNotifications } = await freshStore();
    const stop = watchNotifications();
    const reading = markRead({ run_ids: ['run_a'] });
    firstLoad.resolve(LIST);
    await reading;
    // Not left empty until the next beat: the dropped first load was asked for again.
    expect(notifications.list).toEqual(server);
    expect(live.beats.size).toBe(1);
    stop();
  });

  it('announces what newly arrived, not the first load or a read', async () => {
    api.listNotifications.mockResolvedValueOnce(LIST);
    api.markNotificationsRead.mockResolvedValue({ unread: 0, unread_failed: 0 });
    const { markRead, notifications, refreshNotifications } = await freshStore();
    await refreshNotifications();
    expect(notifications.announcement).toBe('');
    const read = { items: [note('a', true), note('b', true)], unread: 0, unread_failed: 0 };
    api.listNotifications.mockResolvedValueOnce(read);
    await markRead({ ids: ['a'] });
    expect(notifications.announcement).toBe('');
    api.listNotifications.mockResolvedValueOnce({
      items: [note('c', false, 'succeeded'), ...read.items],
      unread: 1,
      unread_failed: 0,
    });
    await refreshNotifications();
    expect(notifications.announcement).toBe('1 new notification; 1 unread');
    // Another arrival with the same counts still changes the live region, so it is read out.
    const first = notifications.announcement;
    api.listNotifications.mockResolvedValueOnce(read);
    api.markNotificationsRead.mockResolvedValue({ unread: 0, unread_failed: 0 });
    await markRead({ ids: ['c'] });
    api.listNotifications.mockResolvedValueOnce({
      items: [note('d', false, 'succeeded'), ...read.items],
      unread: 1,
      unread_failed: 0,
    });
    await refreshNotifications();
    expect(notifications.announcement).not.toBe(first);
    expect(notifications.announcement.trim()).toBe('1 new notification; 1 unread');
  });

  it('keeps a run opened from the bell read until its page’s read settles', async () => {
    api.listNotifications.mockResolvedValue(LIST);
    const reply = deferred<{ unread: number; unread_failed: number }>();
    api.markNotificationsRead.mockReturnValue(reply.promise);
    const { markRead, notifications, openedFromBell, refreshNotifications } = await freshStore();
    await refreshNotifications();
    openedFromBell('run_a');
    expect(notifications.list!.items[0].read).toBe(true);
    expect(api.markNotificationsRead).not.toHaveBeenCalled();
    // A beat before the run's page sends its read: the dot stays cleared.
    await refreshNotifications();
    expect(notifications.list!.items[0].read).toBe(true);
    expect(notifications.list!.unread).toBe(0);
    // The page's read is the one request; once it settles the server's list stands.
    const reading = markRead({ run_ids: ['run_a'] });
    reply.resolve({ unread: 1, unread_failed: 1 });
    await reading;
    expect(api.markNotificationsRead).toHaveBeenCalledTimes(1);
    expect(notifications.list).toEqual(LIST);
  });

  it('lets an opened run’s local read lapse when its page never sends one', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    try {
      api.listNotifications.mockResolvedValue(LIST);
      const { notifications, openedFromBell, refreshNotifications } = await freshStore();
      await refreshNotifications();
      openedFromBell('run_a');
      await refreshNotifications();
      expect(notifications.list!.items[0].read).toBe(true);
      // Thirty seconds on, the server's unread state stands again.
      vi.setSystemTime(Date.now() + 30_001);
      await refreshNotifications();
      expect(notifications.list).toEqual(LIST);
    } finally {
      vi.useRealTimers();
    }
  });
});
