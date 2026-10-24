// The notification bell's words and its list's bookkeeping, kept pure so they are
// unit-tested: how one notification reads in each view, what the bell and its badge say, what a
// read does to the list the bell shows before the server answers, and what newly arrived.

import type { Notification, NotificationList, NotificationsReadRequest } from '#lib/api/index.js';
import { fmtRelative, plural } from '#lib/util/format.js';
import { derivedName, pipelineTitle } from '#lib/runs/runName.js';
import type { ViewMode } from '#lib/view/viewRoutes.js';
import { t } from '#lib/view/vocab.js';

/** How many notifications the bell shows (and asks for). */
export const NOTIFICATION_LIMIT = 20;

/** One notification, as the bell's dropdown shows it. */
export interface NotificationText {
  /** `rnaseq · batch-12 finished` */
  title: string;
  /** `12 samples`, `1 sample failed`; empty when there is nothing to add. */
  detail: string;
  /** `2 h ago` */
  ago: string;
  /** Whether it is about something that went wrong (a failure; drawn in the failure colour). */
  failed: boolean;
}

/**
 * What a failed run did, in `mode`'s words, read or not (the bell shows which are unread). Never
 * "needs attention": that is My work's section, which keeps a failure until it is dismissed,
 * whatever the bell's read state says.
 */
function failedWords(mode: ViewMode): string {
  return mode === 'expert' ? 'failed' : 'stopped with a problem';
}

/**
 * `n` in `mode`'s words: `<pipeline> · <analysis> finished` (Lab) or `<workflow> · <run>
 * succeeded` (Expert), with the samples it counted and how long before `nowSecs` it was. A kind
 * the bell does not know (the field is open) is said plainly.
 */
export function notificationText(
  n: Notification,
  mode: ViewMode,
  nowSecs: number = Date.now() / 1000,
): NotificationText {
  // A run with no name of its own is named by its pipeline, day and samples, which already says
  // the pipeline; one with a name is prefixed by its pipeline's title.
  const own = n.run_name?.trim();
  const pipeline = pipelineTitle(n);
  const derived = own
    ? null
    : derivedName(
        {
          run_id: n.run_id,
          workflow_name: n.workflow_name,
          workflow_title: n.workflow_title,
          submitted_at: n.submitted_at,
          sampleCount: n.samples_total,
        },
        new Date(nowSecs * 1000),
      );
  const subject = own ? (pipeline ? `${pipeline} · ${own}` : own) : (derived ?? n.run_id);
  const total = n.samples_total;
  const failedSamples = n.samples_failed;
  let what: string;
  let detail = '';
  switch (n.kind) {
    case 'succeeded':
      what = t('succeeded', mode);
      if (total) detail = plural(total, 'sample');
      break;
    case 'failed':
      what = failedWords(mode);
      if (failedSamples) detail = `${plural(failedSamples, 'sample')} failed`;
      break;
    case 'cancelled':
      what = 'was cancelled';
      break;
    default:
      what = `· ${n.kind}`;
  }
  return {
    title: `${subject} ${what}`,
    detail,
    ago: fmtRelative(n.at, { now: nowSecs, words: true }),
    failed: n.kind === 'failed',
  };
}

/**
 * The unread count in words, with how many of the unread stopped with a problem (`mode`'s failure
 * words): `3 unread`, `3 unread, 1 stopped with a problem` (Expert: `1 failed`); empty when none.
 */
export function unreadSummary(
  unread: number,
  unreadFailed: number,
  mode: ViewMode = 'lab',
): string {
  if (unread <= 0) return '';
  const failed = Math.min(unreadFailed, unread);
  if (failed <= 0) return `${unread} unread`;
  return `${unread} unread, ${failed} ${failedWords(mode)}`;
}

/** The bell button's accessible name: `Notifications, 3 unread, 1 stopped with a problem`. */
export function bellLabel(unread: number, unreadFailed = 0, mode: ViewMode = 'lab'): string {
  const summary = unreadSummary(unread, unreadFailed, mode);
  return summary ? `Notifications, ${summary}` : 'Notifications';
}

/** The badge's text: the count, capped so it fits the bell (`99+`). */
export function badgeText(unread: number): string {
  return unread > 99 ? '99+' : String(unread);
}

/**
 * How many unread notifications `next` holds that `prev` did not (they arrived since): what the
 * bell announces. None on the first load (`prev` null), and a read never adds one, so the
 * viewer's own reads are never announced.
 */
export function newlyUnread(prev: NotificationList | null, next: NotificationList): number {
  if (!prev) return 0;
  const seen = new Set(prev.items.map((n) => n.id));
  return next.items.filter((n) => !n.read && !seen.has(n.id)).length;
}

/** What the bell's live region says when `arrived` new ones came in:
 * `1 new notification; 3 unread`. */
export function arrivalText(
  arrived: number,
  unread: number,
  unreadFailed: number,
  mode: ViewMode = 'lab',
): string {
  if (arrived <= 0) return '';
  const summary = unreadSummary(unread, unreadFailed, mode);
  const news = plural(arrived, 'new notification');
  return summary ? `${news}; ${summary}` : news;
}

/** The empty dropdown's words. */
export function emptyText(mode: ViewMode): string {
  return `You'll see finished and failed ${t('runsLower', mode)} here.`;
}

/** Whether `read` marks notification `n`. */
function covers(read: NotificationsReadRequest, n: Notification): boolean {
  if (read.all) return true;
  if (read.ids) return read.ids.includes(n.id);
  if (read.run_ids) return read.run_ids.includes(n.run_id);
  return false;
}

/**
 * `list` once `read` is applied, before the server answers: the notifications it names are read,
 * and the unread counts drop by those that were unread (to zero for `all`, which also covers the
 * notifications past the list's end). The server's answer then sets the counts exactly.
 */
export function applyRead(
  list: NotificationList,
  read: NotificationsReadRequest,
): NotificationList {
  let unread = 0;
  let unreadFailed = 0;
  const items = list.items.map((n) => {
    if (n.read || !covers(read, n)) return n;
    unread++;
    if (n.kind === 'failed') unreadFailed++;
    return { ...n, read: true };
  });
  if (read.all) return { items, unread: 0, unread_failed: 0 };
  return {
    items,
    unread: Math.max(0, list.unread - unread),
    unread_failed: Math.max(0, list.unread_failed - unreadFailed),
  };
}
