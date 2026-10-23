// The caller's notifications, shared: the bell in the app shell reads them, and a
// page that marks a run read (the analysis page, the run page) updates the bell's count at once.
//
// Refreshed on the shared 15 s status poll's beat rather than a timer of its own (see
// `onStatusProbe`), so it pauses with it while the tab is hidden. A failed refresh keeps the last
// good list and records why, for the open dropdown to say so.
//
// Reads and refreshes race: a list the server sent while a read was on its way can show the read
// undone. So a refresh that started, or answered, while any read was pending is discarded, and
// once the last pending read settles (taken or refused) the list is asked for again. That one is
// accepted, unless yet another read overtook it, in which case that read's settling asks again.

import {
  listNotifications,
  markNotificationsRead,
  type NotificationList,
  type NotificationsReadRequest,
} from '$lib/api';
import { errorMessage } from '$lib/util/format';
import { applyRead, arrivalText, newlyUnread, NOTIFICATION_LIMIT } from '$lib/notifications/notify';
import { view } from '$lib/state/userPrefs.svelte';
import { onStatusProbe, watchStatus } from '$lib/status/live.svelte';

export const notifications = $state<{
  /** The last list the server gave (with reads applied since); `null` until the first. */
  list: NotificationList | null;
  /** Why the latest refresh failed; `null` once one succeeds. */
  error: string | null;
  /** Why the latest read (marking some read) was refused; `null` once one is taken. */
  readError: string | null;
  /**
   * What newly arrived, for a screen reader (`1 new notification; 3 unread`): set when a refresh
   * brings unread notifications the last list did not hold, never on the first load or a read.
   */
  announcement: string;
}>({ list: null, error: null, readError: null, announcement: '' });

// Reads the server has not answered yet, and a count of reads started: a refresh is accepted only
// when no read was pending while it was out, and a read's counts only when no newer read started.
let pendingReads = 0;
let readsStarted = 0;

// The refresh in flight (one at a time), and whether another was asked for while it was out: it
// runs again when it settles, so an ask is never swallowed by the one already going.
let inFlight: Promise<void> | null = null;
let again = false;

// Runs opened from the bell: read in the bell at once, while the run's page tells the server (its
// read is the one POST). Until that read settles, or a short while passes, a fresh list has them
// applied again, so the dot does not come back in between.
const OPENING_MS = 30_000;
const opening = new Map<string, number>();

function withOpening(list: NotificationList): NotificationList {
  const now = Date.now();
  for (const [run, until] of opening) if (until <= now) opening.delete(run);
  return opening.size ? applyRead(list, { run_ids: [...opening.keys()] }) : list;
}

/** Take a list the server sent: apply what is being opened, and announce what arrived. */
function accept(list: NotificationList) {
  const next = withOpening(list);
  const arrived = newlyUnread(notifications.list, next);
  if (arrived > 0) {
    // The same words as the last arrival would not change the live region, so it would not be
    // read out again: a no-break space keeps them apart.
    const text = arrivalText(arrived, next.unread, next.unread_failed, view.mode);
    notifications.announcement = notifications.announcement === text ? `${text}\u00a0` : text;
  }
  notifications.list = next;
  notifications.error = null;
}

async function refreshOnce(): Promise<void> {
  const started = readsStarted;
  const clean = pendingReads === 0;
  // Still current when it answers: no read was pending when it started, none is now, and none
  // came and went in between.
  const current = () => clean && pendingReads === 0 && readsStarted === started;
  try {
    const list = await listNotifications(NOTIFICATION_LIMIT);
    if (current()) accept(list);
  } catch (e) {
    if (current()) notifications.error = errorMessage(e);
  }
}

/**
 * Ask the server for the latest notifications now. While one is in flight, it is asked again once
 * that settles; the promise settles when the asking is done.
 */
export function refreshNotifications(): Promise<void> {
  if (inFlight) {
    again = true;
    return inFlight;
  }
  inFlight = (async () => {
    try {
      do {
        again = false;
        await refreshOnce();
      } while (again);
    } finally {
      inFlight = null;
    }
  })();
  return inFlight;
}

/**
 * Mark notifications read: at once in the bell (before the server answers), then the server's
 * counts, unless a newer read started meanwhile. Once no read is pending the list is asked for
 * again, so a refused read never leaves the bell claiming it. Resolves to whether the server took
 * the read.
 */
export async function markRead(read: NotificationsReadRequest): Promise<boolean> {
  const mine = ++readsStarted;
  pendingReads++;
  if (notifications.list) notifications.list = applyRead(notifications.list, read);
  let taken = false;
  try {
    const counts = await markNotificationsRead(read);
    taken = true;
    notifications.readError = null;
    if (mine === readsStarted && notifications.list)
      notifications.list = {
        ...notifications.list,
        unread: counts.unread,
        unread_failed: counts.unread_failed,
      };
  } catch (e) {
    notifications.readError = errorMessage(e);
  } finally {
    for (const run of read.run_ids ?? []) opening.delete(run);
    pendingReads--;
  }
  if (pendingReads === 0) await refreshNotifications();
  return taken;
}

/**
 * The viewer opened run `runId` from the bell: show its notifications read at once. The run's page
 * sends the read (`markRead` with its `run_ids`), so opening one is a single request.
 */
export function openedFromBell(runId: string): void {
  opening.set(runId, Date.now() + OPENING_MS);
  if (notifications.list) notifications.list = applyRead(notifications.list, { run_ids: [runId] });
}

let watchers = 0;
let release: (() => void) | undefined;

/**
 * Keep the notifications fresh while anything shows them: the first watcher loads them and joins
 * the status poll's beat, the last to release leaves it. Returns the release (return it from an
 * `$effect`).
 */
export function watchNotifications(): () => void {
  if (watchers++ === 0) {
    const stopStatus = watchStatus();
    const stopBeat = onStatusProbe(() => void refreshNotifications());
    void refreshNotifications();
    release = () => {
      stopBeat();
      stopStatus();
    };
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    if (--watchers === 0) {
      release?.();
      release = undefined;
    }
  };
}
