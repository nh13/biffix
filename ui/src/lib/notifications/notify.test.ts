import { describe, expect, it } from 'vitest';
import type { Notification, NotificationList } from '$lib/api';
import {
  applyRead,
  arrivalText,
  badgeText,
  bellLabel,
  emptyText,
  newlyUnread,
  notificationText,
  unreadSummary,
} from './notify';

const NOW = 1_800_000_000;
const note = (over: Partial<Notification> = {}): Notification => ({
  id: 'n1',
  run_id: 'run_1',
  run_name: 'batch-12',
  workflow_name: 'rnaseq',
  kind: 'succeeded',
  at: NOW - 2 * 3600,
  read: false,
  ...over,
});

describe('notificationText', () => {
  it('speaks the Lab view’s words', () => {
    expect(notificationText(note({ samples_total: 12 }), 'lab', NOW)).toEqual({
      title: 'rnaseq · batch-12 finished',
      detail: '12 samples',
      ago: '2 h ago',
      failed: false,
    });
    const failed = notificationText(
      note({ kind: 'failed', samples_total: 12, samples_failed: 1 }),
      'lab',
      NOW,
    );
    expect([failed.title, failed.detail, failed.failed]).toEqual([
      'rnaseq · batch-12 stopped with a problem',
      '1 sample failed',
      true,
    ]);
    expect(notificationText(note({ kind: 'cancelled' }), 'lab', NOW).title).toBe(
      'rnaseq · batch-12 was cancelled',
    );
  });

  it('says a failure stopped with a problem, read or not, never that it needs attention', () => {
    // "Needs attention" is My work's section (until dismissed), not the bell's read state.
    for (const read of [false, true]) {
      const text = notificationText(note({ kind: 'failed', read }), 'lab', NOW);
      expect([text.title, text.failed]).toEqual(['rnaseq · batch-12 stopped with a problem', true]);
    }
    // Expert says failed, read or not.
    expect(notificationText(note({ kind: 'failed', read: true }), 'expert', NOW).title).toBe(
      'rnaseq · batch-12 failed',
    );
  });

  it('names a run without a name by its pipeline, start and samples, and prefers the title', () => {
    const noon = new Date(new Date(NOW * 1000).setHours(12, 0, 0, 0)).getTime() / 1000;
    const day = `${new Date(noon * 1000).toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
    })} 12:00`;
    const unnamed = note({ run_name: null, submitted_at: noon, samples_total: 2 });
    expect(notificationText(unnamed, 'lab', NOW).title).toBe(
      `rnaseq · ${day} · 2 samples finished`,
    );
    const titled = note({ run_name: null, workflow_title: 'Bulk RNA-seq', submitted_at: noon });
    expect(notificationText(titled, 'lab', NOW).title).toBe(`Bulk RNA-seq · ${day} finished`);
    expect(notificationText(note({ workflow_title: 'Bulk RNA-seq' }), 'lab', NOW).title).toBe(
      'Bulk RNA-seq · batch-12 finished',
    );
    // Nothing known: the id, as before.
    expect(notificationText(note({ run_name: null, workflow_name: null }), 'lab', NOW).title).toBe(
      'run_1 finished',
    );
  });

  it('says how long ago in words', () => {
    expect(notificationText(note({ at: NOW - 30 * 3600 }), 'lab', NOW).ago).toBe('yesterday');
    expect(notificationText(note({ at: NOW - 21 * 86400 }), 'lab', NOW).ago).toBe('21 days ago');
  });

  it('speaks the Expert view’s words', () => {
    expect(notificationText(note(), 'expert', NOW).title).toBe('rnaseq · batch-12 succeeded');
    const failed = notificationText(note({ kind: 'failed', samples_failed: 3 }), 'expert', NOW);
    expect([failed.title, failed.detail]).toEqual(['rnaseq · batch-12 failed', '3 samples failed']);
  });

  it('falls back to the run id, and says an unknown kind plainly', () => {
    const bare = note({ run_name: null, workflow_name: null, kind: 'archived' });
    expect(notificationText(bare, 'lab', NOW)).toMatchObject({
      title: 'run_1 · archived',
      detail: '',
    });
    // No sample counts: nothing more to say.
    expect(notificationText(note({ kind: 'failed' }), 'lab', NOW).detail).toBe('');
  });
});

describe('the bell', () => {
  it('names the unread count, and caps the badge', () => {
    expect(bellLabel(0)).toBe('Notifications');
    expect(bellLabel(3)).toBe('Notifications, 3 unread');
    expect(bellLabel(3, 1)).toBe('Notifications, 3 unread, 1 stopped with a problem');
    expect(bellLabel(3, 2)).toBe('Notifications, 3 unread, 2 stopped with a problem');
    expect(bellLabel(3, 2, 'expert')).toBe('Notifications, 3 unread, 2 failed');
    expect(bellLabel(0, 0)).toBe('Notifications');
    expect(badgeText(7)).toBe('7');
    expect(badgeText(120)).toBe('99+');
  });

  it('says what will appear, in each view’s words', () => {
    expect(emptyText('lab')).toBe("You'll see finished and failed analyses here.");
    expect(emptyText('expert')).toBe("You'll see finished and failed runs here.");
  });
});

describe('unreadSummary', () => {
  it('says how many are unread and how many of those stopped with a problem', () => {
    expect(unreadSummary(0, 0)).toBe('');
    expect(unreadSummary(2, 0)).toBe('2 unread');
    expect(unreadSummary(2, 1)).toBe('2 unread, 1 stopped with a problem');
    expect(unreadSummary(2, 1, 'expert')).toBe('2 unread, 1 failed');
    // Never more failed than are unread (counts caught mid-update).
    expect(unreadSummary(1, 3)).toBe('1 unread, 1 stopped with a problem');
  });
});

describe('what arrived', () => {
  const list = (items: Notification[]): NotificationList => ({
    items,
    unread: items.filter((n) => !n.read).length,
    unread_failed: 0,
  });
  const a = note({ id: 'a' });
  const b = note({ id: 'b', kind: 'failed' });

  it('counts the unread ones the last list did not hold, and none on the first load', () => {
    expect(newlyUnread(null, list([a, b]))).toBe(0);
    expect(newlyUnread(list([a]), list([b, a]))).toBe(1);
    // One that arrived already read (read elsewhere) is not news.
    expect(newlyUnread(list([a]), list([{ ...b, read: true }, a]))).toBe(0);
    // A read of one already listed is not news either.
    expect(newlyUnread(list([a, b]), list([{ ...a, read: true }, b]))).toBe(0);
  });

  it('says what arrived and where the count stands', () => {
    expect(arrivalText(0, 2, 0)).toBe('');
    expect(arrivalText(1, 3, 0)).toBe('1 new notification; 3 unread');
    expect(arrivalText(2, 3, 1)).toBe('2 new notifications; 3 unread, 1 stopped with a problem');
    expect(arrivalText(1, 3, 1, 'expert')).toBe('1 new notification; 3 unread, 1 failed');
  });
});

describe('applyRead', () => {
  const list: NotificationList = {
    items: [
      note({ id: 'a', run_id: 'r1', kind: 'failed' }),
      note({ id: 'b', run_id: 'r1', kind: 'succeeded', read: true }),
      note({ id: 'c', run_id: 'r2' }),
    ],
    // More unread than are listed: the older ones past the bell's 20.
    unread: 5,
    unread_failed: 2,
  };
  const readIds = (l: NotificationList) => l.items.filter((n) => n.read).map((n) => n.id);

  it('marks the named notifications and drops the counts by those that were unread', () => {
    const byId = applyRead(list, { ids: ['a', 'b'] });
    expect(readIds(byId)).toEqual(['a', 'b']);
    expect([byId.unread, byId.unread_failed]).toEqual([4, 1]);
    const byRun = applyRead(list, { run_ids: ['r2'] });
    expect(readIds(byRun)).toEqual(['b', 'c']);
    expect([byRun.unread, byRun.unread_failed]).toEqual([4, 2]);
  });

  it('clears everything for all, the notifications past the list included', () => {
    const all = applyRead(list, { all: true });
    expect(readIds(all)).toEqual(['a', 'b', 'c']);
    expect([all.unread, all.unread_failed]).toEqual([0, 0]);
  });

  it('changes nothing for a read of nothing it holds, and never counts below zero', () => {
    expect(applyRead(list, { ids: ['zzz'] })).toEqual(list);
    const low = applyRead({ ...list, unread: 0, unread_failed: 0 }, { ids: ['a'] });
    expect([low.unread, low.unread_failed]).toEqual([0, 0]);
    // The list it was given is left as it was.
    expect(readIds(list)).toEqual(['b']);
  });
});
