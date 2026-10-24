// How the sidebar's Status icon shows the service health (`#lib/status/health.js` gives the verdict).

import type { Health } from '#lib/status/health.js';

/** The icon's color for a verdict (the page's text color while unknown). */
export function healthColor(health: Health): string {
  switch (health) {
    case 'ok':
      return '#30a46c';
    case 'warn':
      return '#f5a524';
    case 'bad':
      return '#e5484d';
    default:
      return 'currentColor';
  }
}

/** The verdict in words, for those who don't see the icon's color ('' while unknown). */
export function healthWords(health: Health): string {
  switch (health) {
    case 'ok':
      return 'working';
    case 'warn':
      return 'degraded';
    case 'bad':
      return 'down or unreachable';
    default:
      return '';
  }
}
