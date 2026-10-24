// Switching view from a page: the one way the view toggle and the Expert hint both do it.

import { goto } from '$app/navigation';
import { page } from '$app/state';
import { setView } from '#lib/state/userPrefs.svelte.js';
import { toggleTarget, type ViewMode } from '#lib/view/viewRoutes.js';

/** Switch to `mode` (saved for the user) and go to this page's place in it, if it has another. */
export async function switchViewTo(mode: ViewMode): Promise<void> {
  void setView(mode);
  const path = page.url.pathname;
  const target = toggleTarget(path, mode);
  if (target !== path) await goto(target);
}
