// The site chrome (title + logo) every page shows, shared so a save on the Settings page shows in
// the sidebar, the top bar, the document title and the favicon at once rather than after a reload.
// The root layout loads it once per session and applies the document side effects; the Settings
// page hands it what the server stored after a save.

import { getSettings, type SiteSettings } from '$lib/api';

export const DEFAULT_SITE_TITLE = 'biffix';

/**
 * `null` until loaded (or when the load failed): the chrome then shows the defaults. `pageTitle` is
 * what the page shown names itself (an analysis's name), put before the site's title in the tab.
 */
export const site = $state<{ title: string | null; logo: string | null; pageTitle: string | null }>(
  {
    title: null,
    logo: null,
    pageTitle: null,
  },
);

/** Show these settings (what `GET /settings` or `PUT /settings` returned). */
export function setSite(s: SiteSettings) {
  site.title = s.title ?? null;
  site.logo = s.logo_data_url ?? null;
}

/** Load the settings into the chrome; a failure keeps the defaults (the chrome is optional). */
export async function loadSite(): Promise<void> {
  try {
    setSite(await getSettings());
  } catch {
    /* settings are optional chrome; the defaults stand */
  }
}
