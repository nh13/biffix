import { goto } from '$app/navigation';
import { page } from '$app/state';

/**
 * Replace the current history entry with `url` without scrolling or losing focus — used by the
 * list/detail views that push filter/sort/tab state into the query string. One definition so the
 * call sites can't drift on the option flags.
 */
export function replaceUrl(url: URL | string): Promise<void> {
  return goto(url, { replace: true, reset: false });
}

/** The current URL with query param `name` set to `value` (removed for null or empty), replacing
 * the history entry (`replaceUrl`). */
export function replaceQueryParam(name: string, value: string | null): Promise<void> {
  const u = new URL(page.url.href);
  if (value) u.searchParams.set(name, value);
  else u.searchParams.delete(name);
  return replaceUrl(u);
}
