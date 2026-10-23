// The sidebar's nav entries, per view. An entry is current on its own path and below it (Home only
// on `/` itself).

import type { ComponentProps } from 'svelte';
import type Icon from '$lib/components/Icon.svelte';
import { t } from '$lib/view/vocab';
import type { ViewMode } from '$lib/view/viewRoutes';

/**
 * A nav entry. `icon` is an <Icon> name; `health` flags the one whose icon is tinted by overall
 * service health (the activity glyph on /status).
 */
export type NavItem = {
  href: string;
  label: string;
  icon: ComponentProps<typeof Icon>['name'];
  health?: boolean;
};

// The Lab view's nav: the bench's words, and only what a biologist needs. The Expert view's pages
// stay reachable by URL (with a banner) and by switching view.
const LAB_NAV: readonly NavItem[] = [
  { href: '/', label: t('home', 'lab'), icon: 'home' },
  { href: '/runs', label: t('runs', 'lab'), icon: 'runs' },
  { href: '/workflows', label: t('workflows', 'lab'), icon: 'workflows' },
  { href: '/experiments', label: 'Experiments', icon: 'experiments' },
  { href: '/data', label: 'Data', icon: 'data' },
  { href: '/settings', label: 'Settings', icon: 'settings' },
];
const EXPERT_NAV: readonly NavItem[] = [
  { href: '/', label: t('home', 'expert'), icon: 'home' },
  { href: '/runs', label: t('runs', 'expert'), icon: 'runs' },
  { href: '/workflows', label: t('workflows', 'expert'), icon: 'workflows' },
  { href: '/experiments', label: 'Experiments', icon: 'experiments' },
  { href: '/data', label: 'Data', icon: 'data' },
  { href: '/usage', label: 'Usage', icon: 'usage' },
  { href: '/status', label: 'Status', icon: 'activity', health: true },
  { href: '/api-docs', label: 'API', icon: 'api' },
  { href: '/settings', label: 'Settings', icon: 'settings' },
];

/** The nav entries of `mode`'s sidebar. */
export function navFor(mode: ViewMode): readonly NavItem[] {
  return mode === 'lab' ? LAB_NAV : EXPERT_NAV;
}

/** Whether the entry at `href` is current on `path`: its own path and below it, Home only on `/`. */
export function isCurrent(href: string, path: string): boolean {
  return href === '/' ? path === '/' : path === href || path.startsWith(`${href}/`);
}
