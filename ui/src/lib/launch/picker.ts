// Pure helpers for the file picker: where a URI lives among the data sources, and the file-type
// filter. (Breadcrumbs and where an upload goes, shared with the Data page, are in #lib/data and
// #lib/upload.)

import type { DataSource } from '#lib/api/index.js';

/** Whether a file name ends in one of `suffixes` (case-insensitive); no suffixes ⇒ any. */
export function matchesSuffix(name: string, suffixes: readonly string[]): boolean {
  if (suffixes.length === 0) return true;
  const lower = name.toLowerCase();
  return suffixes.some((s) => lower.endsWith(s.toLowerCase()));
}

/** The folder of a path relative to a source root: `'a/b/c.txt'` → `'a/b/'`. */
export function parentFolder(path: string): string {
  const i = path.replace(/\/$/, '').lastIndexOf('/');
  return i < 0 ? '' : path.slice(0, i + 1);
}

/**
 * The data source holding `uri`, and the folder to open it at: the URI's own folder (a file's
 * parent, or the folder itself when `uri` ends in `/`). The most specific (longest) source wins
 * when sources overlap.
 */
export function locate(
  sources: readonly DataSource[],
  uri: string,
): { source: DataSource; folder: string } | null {
  const holders = sources
    .filter((s) => uri.startsWith(s.uri))
    .sort((a, b) => b.uri.length - a.uri.length);
  const source = holders[0];
  if (!source) return null;
  const rel = uri.slice(source.uri.length);
  return { source, folder: rel.endsWith('/') ? rel : parentFolder(rel) };
}

/** Whether `text` looks like an `s3://bucket/key` URI. */
export function isS3Uri(text: string): boolean {
  return /^s3:\/\/[^/\s]+\/\S*$/.test(text.trim());
}
