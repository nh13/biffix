// Whether the Lab view's "Looking for logs and task details? Switch to Expert" line is
// shown: until the viewer dismisses it, once, for every page that has it. Kept in
// this browser (a per-viewer convenience), so a colleague who opened a shared analysis sees it, and
// it never comes back after a dismissal; Settings can bring it back.

import { readPref, writePref } from '$lib/util/storage';

const KEY = 'biffix-expert-hint-dismissed';

export const expertHint = $state<{ dismissed: boolean }>({
  dismissed: readPref(KEY) === 'true',
});

/** Hide the line everywhere, from now on. */
export function dismissExpertHint(): void {
  expertHint.dismissed = true;
  writePref(KEY, 'true');
}

/** Show the line again (Settings). */
export function restoreExpertHint(): void {
  expertHint.dismissed = false;
  writePref(KEY, '');
}
