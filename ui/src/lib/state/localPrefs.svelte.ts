// Per-viewer display preferences, kept in this browser's localStorage: the theme, the sidebar and
// the log order. Deliberately NOT site settings: a display preference is one person's taste, so it
// must not be forced on every viewer. The shared `$state` makes a change on the Settings page apply
// live to any component reading it (e.g. an open task-log panel).

import { readPref, removePref, writePref } from '$lib/util/storage';

export type LogOrder = 'oldest' | 'newest';

const LOG_ORDER_KEY = 'biffix-log-order';

// The Workflows pages' Developer filter: on, they list every pipeline, developer versions included;
// off (the default), only pipelines with a released version.
const DEVELOPER_KEY = 'biffix-show-developer';

export const prefs = $state<{ logOrder: LogOrder; showDeveloper: boolean }>({
  logOrder: readPref(LOG_ORDER_KEY) === 'newest' ? 'newest' : 'oldest',
  showDeveloper: readPref(DEVELOPER_KEY) === 'true',
});

export function setLogOrder(order: LogOrder) {
  prefs.logOrder = order;
  writePref(LOG_ORDER_KEY, order);
}

/** Turn the Workflows pages' Developer filter on or off, remembered in this browser. */
export function setShowDeveloper(on: boolean) {
  prefs.showDeveloper = on;
  writePref(DEVELOPER_KEY, String(on));
}

export type Theme = 'light' | 'dark';

// app.html reads the same key before first paint.
const THEME_KEY = 'biffix-theme';

/** The theme the viewer chose with the toggle, or `null` to follow the OS. */
export function savedTheme(): Theme | null {
  const t = readPref(THEME_KEY);
  return t === 'light' || t === 'dark' ? t : null;
}

/** Record the viewer's choice of theme; from then on it wins over the OS. */
export function saveTheme(theme: Theme): void {
  writePref(THEME_KEY, theme);
}

/** A theme choice: one of the themes, or `system` to follow the OS. */
export type ThemeChoice = Theme | 'system';

/**
 * The viewer's theme choice, shared: the sidebar's toggle and the Settings page change it, and the
 * app shell applies it (following the OS while it is `system`).
 */
export const appearance = $state<{ theme: ThemeChoice }>({ theme: savedTheme() ?? 'system' });

/** Choose a theme, or `system` to follow the OS again (app.html's pre-paint script then does too). */
export function setThemeChoice(choice: ThemeChoice): void {
  appearance.theme = choice;
  if (choice === 'system') removePref(THEME_KEY);
  else saveTheme(choice);
}

const SIDEBAR_KEY = 'biffix-sidebar-collapsed';

export function savedSidebarCollapsed(): boolean {
  return readPref(SIDEBAR_KEY) === 'true';
}

export function saveSidebarCollapsed(collapsed: boolean): void {
  writePref(SIDEBAR_KEY, String(collapsed));
}
