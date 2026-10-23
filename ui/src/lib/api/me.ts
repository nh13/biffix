// The caller's own state: display preferences, notifications and *Needs attention*.

import type { components } from '$lib/generated/api';
import { API_PATHS, get, requestJson, withQuery, type JsonBody } from './client';

/** The caller's own display preferences (Lab/Expert view, pins, tour). */
export type UserPrefs = JsonBody<'get_prefs'>;
/** `PUT /me/prefs` — a partial update: a field left out keeps its stored value. */
export type UserPrefsUpdateRequest = components['schemas']['UserPrefsUpdateRequest'];

export function getUserPrefs(): Promise<UserPrefs> {
  return get<UserPrefs>(API_PATHS.mePrefs);
}

/** Update the caller's preferences; resolves to what the server now stores. */
export function putUserPrefs(update: UserPrefsUpdateRequest): Promise<UserPrefs> {
  return requestJson<UserPrefs>(API_PATHS.mePrefs, { method: 'PUT', body: update });
}

export type Notification = components['schemas']['Notification'];
export type NotificationList = components['schemas']['NotificationList'];
/** Give exactly one of `ids`, `run_ids` or `all: true`. */
export type NotificationsReadRequest = components['schemas']['NotificationsReadRequest'];
export type UnreadCounts = components['schemas']['UnreadCounts'];

/** The caller's latest `limit` notifications, newest first, with their unread counts. */
export function listNotifications(limit = 20): Promise<NotificationList> {
  return get<NotificationList>(
    withQuery(API_PATHS.notifications, new URLSearchParams({ limit: String(limit) })),
  );
}

/** Mark some of the caller's notifications read; resolves to the unread counts after it. */
export function markNotificationsRead(read: NotificationsReadRequest): Promise<UnreadCounts> {
  return requestJson<UnreadCounts>(API_PATHS.notificationsRead, {
    method: 'POST',
    body: read,
  });
}

export type AttentionCount = components['schemas']['AttentionCount'];

/**
 * Take the caller's failed runs `runIds` out of their *Needs attention* (exactly those: one row,
 * or every run *Dismiss all* showed). Not a read: the bell's unread state is untouched. Resolves
 * to how many still need attention.
 */
export function dismissAttention(runIds: string[]): Promise<AttentionCount> {
  return requestJson<AttentionCount>(API_PATHS.attentionDismiss, {
    method: 'POST',
    body: { run_ids: runIds } satisfies components['schemas']['AttentionDismissRequest'],
  });
}
