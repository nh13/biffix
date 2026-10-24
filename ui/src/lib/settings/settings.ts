// The Settings page's pure parts: what a site save sends, whether the support contact changed, the
// logo's size limit, and the words for an alert recipient's status and the unread count.

import type { SiteSettingsUpdateRequest, SiteSettings } from '#lib/api/index.js';

// The server caps the logo's data URL (not the file) at 512 KiB; base64 makes a file about 4/3 its
// size, so the largest file that fits is about 384 KB.
export const MAX_DATA_URL_CHARS = 512 * 1024;
export const MAX_FILE_KB = Math.floor((MAX_DATA_URL_CHARS * 3) / 4 / 1024);

/** Why a logo file of `bytes` is refused. */
export function logoTooLarge(bytes: number): string {
  return `Image too large (${Math.round(bytes / 1024)} KB; the limit is about ${MAX_FILE_KB} KB).`;
}

/**
 * The partial update the site form describes: only the fields changed from what is stored, so a
 * title-only edit cannot clear a logo the page never showed.
 */
export function siteChanges(
  stored: SiteSettings | null,
  title: string,
  logo: string | null,
): SiteSettingsUpdateRequest {
  if (!stored) return {};
  const out: SiteSettingsUpdateRequest = {};
  const t = title.trim() || null;
  if (t !== (stored.title ?? null)) out.title = t;
  if (logo !== (stored.logo_data_url ?? null)) out.logo_data_url = logo;
  return out;
}

/** The support contact as typed. */
export interface SupportFields {
  name: string;
  email: string;
  url: string;
}

/** Whether the support contact as typed differs from what is stored. */
export function supportChanged(stored: SiteSettings | null, f: SupportFields): boolean {
  return (
    !!stored &&
    (f.name.trim() !== (stored.support?.name ?? '') ||
      f.email.trim() !== (stored.support?.email ?? '') ||
      f.url.trim() !== (stored.support?.url ?? ''))
  );
}

// An alert recipient's status is an open string; one this page doesn't know shows as itself.
const RECIPIENT_STATUS: Record<string, string> = {
  pending: 'awaiting confirmation',
  confirmed: 'confirmed',
};

/** The words for an alert recipient's status. */
export function recipientStatus(status: string): string {
  return RECIPIENT_STATUS[status] ?? status;
}

/** The unread count, said (nothing until it has loaded). */
export function unreadText(n: number | null): string {
  return n == null ? '' : n === 0 ? 'Nothing unread.' : `${n} unread.`;
}
