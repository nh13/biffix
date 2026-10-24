// Paths within a data source, shared by the file picker and the Data page: the breadcrumb trail of
// a folder, and the Data page URL for a source, folder and file.
//
// Folder and file names can name a sample, so the Data page keeps them in the URL fragment, which
// the browser never sends to a server: `/data/<source>#path=<folder>&file=<name>`. Only the source
// id (chosen by the deploy) is in the path. Carried verbatim, a folder whose name is empty or a
// dot (`a//`, `../`) round-trips too.

import type { ReadonlyURL } from '$app/state';
import { viewParams, withViewParams } from '#lib/util/urlParam.js';

export interface Crumb {
  label: string;
  /** Folder path relative to the source root (`''` is the root). */
  path: string;
}

/** The folder part of a path (`a/b/c.txt` → `a/b/`; `c.txt` → `''`; `a/b/` → itself). */
export function folderOf(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut < 0 ? '' : path.slice(0, cut + 1);
}

/** A folder path as the listing wants it: `''`, or ending in `/`. */
function asFolder(path: string): string {
  return path && !path.endsWith('/') ? `${path}/` : path;
}

/**
 * The trail from a source's root to `path` (`'a/b/'` → root, a, b). Every segment is a folder, an
 * empty one (from a key like `runs//x`) included, so each crumb opens the prefix it names.
 */
export function breadcrumbs(rootLabel: string, path: string): Crumb[] {
  const crumbs: Crumb[] = [{ label: rootLabel, path: '' }];
  if (!path) return crumbs;
  let acc = '';
  for (const part of asFolder(path).slice(0, -1).split('/')) {
    acc += `${part}/`;
    crumbs.push({ label: part || '(empty name)', path: acc });
  }
  return crumbs;
}

/**
 * Where the Data page is for a folder of a source (`path` `''` or ending in `/`), and a file in it.
 * `project` (in the query: an id the deploy chose, never a sample's name) names a project other
 * than the deploy's default one, whose sources these are.
 */
export function dataHref(
  sourceId: string,
  path: string,
  file?: string,
  project?: string | null,
): string {
  const view = new URLSearchParams();
  if (path) view.set('path', path);
  if (file) view.set('file', file);
  return withViewParams(`/data/${encodeURIComponent(sourceId)}${projectQuery(project)}`, view);
}

/** The Data page at the first source of `project` (`null`: the deploy's default project). */
export function dataProjectHref(project: string | null): string {
  return `/data${projectQuery(project)}`;
}

const projectQuery = (project: string | null | undefined) =>
  project ? `?${new URLSearchParams({ project })}` : '';

/** A project as the Data page's project choice knows it. */
export interface ProjectChoice {
  id: string;
  default: boolean;
}

/**
 * Whether the Data page offers a choice of project: only on a deploy with more than one, so a
 * deploy of one project looks as it always has.
 */
export function offersProjectChoice(projects: readonly ProjectChoice[] | null): boolean {
  return (projects?.length ?? 0) > 1;
}

/**
 * The project the Data page names in its reads and links: the URL's `project`, or `null` (the
 * deploy's default project) when it names none.
 */
export function requestedProject(url: ReadonlyURL): string | null {
  return url.searchParams.get('project')?.trim() || null;
}

/** The project the selector shows as chosen: the requested one, else the default one. */
export function chosenProject(
  requested: string | null,
  projects: readonly ProjectChoice[] | null,
): string | null {
  return requested ?? projects?.find((p) => p.default)?.id ?? null;
}

/** What a Data page URL names: the source, the folder, and the file open in it. */
export interface DataLocation {
  sourceId: string | null;
  /** The folder, relative to the source root (`''` or ending in `/`). */
  path: string;
  /** The open file, relative to the folder. */
  file: string | null;
}

/**
 * The location a Data page URL names. `rest` is the route param after `/data/` (the source id),
 * which SvelteKit has already decoded (so it is not decoded again: a literal `%` stays).
 */
export function dataLocation(url: ReadonlyURL, rest: string): DataLocation {
  const view = viewParams(url);
  const path = view.get('path');
  return {
    sourceId: rest.replace(/^\/+|\/+$/g, '') || null,
    path: path !== null ? asFolder(path) : '',
    file: view.get('file') || null,
  };
}
