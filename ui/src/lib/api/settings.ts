// Site settings and the operational-alert recipients.

import type { components } from '#lib/generated/api.js';
import { API_PATHS, DEVELOPER_ONLY, get, request, requestJson, type JsonBody } from './client';

export type SiteSettings = JsonBody<'get_settings'>;

export function getSettings(): Promise<SiteSettings> {
  return get<SiteSettings>(API_PATHS.settings);
}

/** `PUT /settings` — a partial update: a field left out keeps its stored value, `null` clears it. */
export type SiteSettingsUpdateRequest = components['schemas']['SiteSettingsUpdateRequest'];

/** Update the site settings; resolves to what the server now stores. */
export function putSettings(update: SiteSettingsUpdateRequest): Promise<SiteSettings> {
  return requestJson<SiteSettings>(API_PATHS.settings, { method: 'PUT', body: update });
}

export type AlertRecipientList = JsonBody<'get_alert_recipients'>;

/** The operational-alert recipients (developer only). */
export function getAlertRecipients(): Promise<AlertRecipientList> {
  return requestJson<AlertRecipientList>(API_PATHS.alerts, { forbidden: DEVELOPER_ONLY });
}

/** Subscribe an email to alerts; AWS emails it a confirmation link. Returns the updated list. */
export function addAlertRecipient(email: string): Promise<AlertRecipientList> {
  return requestJson<AlertRecipientList>(API_PATHS.alerts, {
    method: 'POST',
    body: { email } satisfies components['schemas']['AlertRecipientAddRequest'],
    forbidden: DEVELOPER_ONLY,
  });
}

/** Remove a confirmed recipient. The address goes in the body (`POST /settings/alerts/remove`),
 * not the request URL, so access logs don't keep it. */
export async function removeAlertRecipient(email: string): Promise<void> {
  await request(API_PATHS.alertRemove, {
    method: 'POST',
    body: { email } satisfies components['schemas']['AlertRecipientRemoveRequest'],
    forbidden: DEVELOPER_ONLY,
  });
}
