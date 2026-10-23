import type { ReadonlyURL, ReadonlyURLSearchParams } from '$app/state';

/**
 * Validate a raw URL search-param against a known set, returning `fallback` when it is absent or
 * unrecognized. Routes read this inside a `$derived` so the result still recomputes on navigation.
 * Centralizes the "membership check + fallback" that would otherwise be repeated with double casts.
 */
export function validParam<T extends string, F extends T | null>(
  raw: string | null,
  allowed: readonly T[],
  fallback: F,
): T | F {
  return (allowed as readonly string[]).includes(raw ?? '') ? (raw as T) : fallback;
}

/** The value a page writes for a boolean URL flag that is on (`?mine=true`): the API's spelling,
 * so a page's flags read the same as the requests behind it. */
export const FLAG_ON = 'true';

/** Whether boolean URL flag `name` is on ([`FLAG_ON`]); anything else, or its absence, is off. */
export function flagParam(params: ReadonlyURLSearchParams, name: string): boolean {
  return params.get(name) === FLAG_ON;
}

/**
 * A page's shareable view state (filters, search text, a selected job, output or file): it can
 * name a sample, a task or a file, so it lives in the URL fragment, which the browser never sends
 * to a server (no PHI in request URLs or server logs). A page may keep view state that names
 * nothing in the query (the Outputs tab's `view` and `work`), so query values fill in; fragment
 * values win.
 */
export function viewParams(url: ReadonlyURL): URLSearchParams {
  const params = new URLSearchParams(url.search);
  for (const [key, value] of new URLSearchParams(url.hash.slice(1))) params.set(key, value);
  return params;
}

/**
 * `path` with `params` as its fragment (none when empty). Build every link that carries view state
 * with this, so no identifier reaches a query string.
 */
export function withViewParams(path: string, params: URLSearchParams): string {
  const fragment = params.toString();
  return fragment ? `${path}#${fragment}` : path;
}
