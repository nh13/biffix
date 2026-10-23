// Where the Data page shows an S3 object or folder: the data source that holds it, the folder in
// it and the file, or nothing when no source of the project shows it (another bucket, or a prefix
// biffix keeps to itself). Pure, so it is unit-tested; the run page links its outputs and output
// prefix with it, beside their copy button. Which prefixes a source hides is the server's answer,
// read off the source, never a copy of its rule.

import { listDataSources, type DataSource } from '$lib/api';
import { dataHref, folderOf } from './paths';

/** A source's hide rule, as the server serves it on the source. */
export type HideRule = Pick<DataSource, 'hidden_prefixes' | 'browsable_exceptions'>;
type SourceLike = Pick<DataSource, 'id' | 'uri'> & HideRule;

/**
 * Whether the Data page hides `path` (relative to `source`'s root): the rule the server serves on
 * the source (`hidden_prefixes`, except `browsable_exceptions`; `DataSource::hides`), which answers
 * 404 for such a path, so a link there would open onto an error.
 */
export function hiddenIn(source: HideRule, path: string): boolean {
  return (
    (source.hidden_prefixes ?? []).some((p) => path.startsWith(p)) &&
    !(source.browsable_exceptions ?? []).some((p) => path.startsWith(p))
  );
}

/** A place in the Data page: a source, a folder in it (`''` or ending in `/`), and a file in it. */
export interface DataSpot {
  sourceId: string;
  path: string;
  /** The file's name in `path`; null for a folder. */
  file: string | null;
}

/**
 * Where the Data page shows `uri` (an `s3://` object, or a folder when it ends in `/`): in the
 * source whose root holds it (the deepest, when several do). Null when none of `sources` holds it,
 * or the one that does hides it.
 */
export function dataSpotOf(
  uri: string | null | undefined,
  sources: readonly SourceLike[],
): DataSpot | null {
  if (!uri || !/^s3:\/\/[^/]+\//.test(uri)) return null;
  let best: { source: SourceLike; rel: string } | null = null;
  for (const source of sources) {
    const root = source.uri.endsWith('/') ? source.uri : `${source.uri}/`;
    if (!uri.startsWith(root)) continue;
    const rel = uri.slice(root.length);
    if (!best || rel.length < best.rel.length) best = { source, rel };
  }
  if (!best || hiddenIn(best.source, best.rel)) return null;
  const path = folderOf(best.rel);
  return { sourceId: best.source.id, path, file: best.rel.slice(path.length) || null };
}

/**
 * The Data page's URL for `uri` in `project` (`null`: the default project), or null when the Data
 * page cannot show it ([`dataSpotOf`]).
 */
export function dataBrowserHref(
  uri: string | null | undefined,
  sources: readonly SourceLike[],
  project?: string | null,
): string | null {
  const spot = dataSpotOf(uri, sources);
  return spot ? dataHref(spot.sourceId, spot.path, spot.file ?? undefined, project) : null;
}

// Each project's sources, asked once per page load and shared by every link on the page (an
// outputs list links each row); a failed request is forgotten, so the next one asks again. The
// Data page itself asks afresh on each visit (`ProjectSources` over `listDataSources`).
const cache = new Map<string, Promise<DataSource[]>>();

/** `project`'s data sources (`null`: the default project's), from the page's cache. */
export function cachedDataSources(project: string | null | undefined): Promise<DataSource[]> {
  const key = project ?? '';
  let sources = cache.get(key);
  if (!sources) {
    sources = listDataSources(project).catch((e) => {
      cache.delete(key);
      throw e;
    });
    cache.set(key, sources);
  }
  return sources;
}

/** Forget the cached sources (tests). */
export function clearDataSourcesCache(): void {
  cache.clear();
}

/**
 * Whether any data folder is connected: a source of kind `data` (a sequencing core's deliveries,
 * say). The project's own bucket is always listed, so its presence says nothing either way.
 */
export function hasDataFolders(sources: readonly Pick<DataSource, 'kind'>[]): boolean {
  return sources.some((s) => s.kind === 'data');
}
