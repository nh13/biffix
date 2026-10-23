// A frozen workflow version's identity is `${git_sha}:${content_hash}:${build}` (the `id`). Two
// registrations of the same commit with different content/images share a git_sha and differ only
// in the content hash, and the same source registered again with other inputs differs only in its
// build, so a version label built from git_sha alone can't tell them apart: it carries the
// content-hash part and the build too. This is the one label scheme: workflow pages and run pages
// alike use it.

import type { WorkflowSummary } from '#lib/api/index.js';

type VersionLike = { id: string; git_sha: string };

const short = (s: string, n: number) => (s.length > n ? s.slice(0, n) : s);
/** How much of the git sha and of the content hash a version label shows. */
const SHA_LEN = 10;
const HASH_LEN = 12;

/** A label from a version's parts: `${short sha}:${short hash}:${build}` (`rest` is `hash:build`). */
function labelOf(sha: string, rest: string): string {
  const cut = rest.lastIndexOf(':');
  const [hash, build] = cut >= 0 ? [rest.slice(0, cut), rest.slice(cut)] : [rest, ''];
  return `${short(sha, SHA_LEN)}:${short(hash, HASH_LEN)}${build}`;
}

/**
 * A distinguishable short version label: `${short git_sha}:${short content_hash}:${build}`. The
 * content hash is the disambiguator between two registrations of the same commit, so it is kept
 * generous (12 chars) — enough to preserve human content labels like `v4-imgfix` whole and to
 * keep real hex hashes distinct — and the build tells apart the builds of one source.
 */
export function versionLabel(v: VersionLike): string {
  const prefix = `${v.git_sha}:`;
  return v.id.startsWith(prefix)
    ? labelOf(v.git_sha, v.id.slice(prefix.length))
    : short(v.id, SHA_LEN + 1 + HASH_LEN);
}

/**
 * [`versionLabel`] for a bare version id (a run names only its version's id): the git sha is the
 * id's first `:`-part. An id without one is shown whole.
 */
export function versionLabelOfId(id: string): string {
  const cut = id.indexOf(':');
  return cut > 0 ? labelOf(id.slice(0, cut), id.slice(cut + 1)) : id;
}

/**
 * A version's registrant, for display: the human label if the backend resolved one, else the raw
 * actor id, else null. Shared by the workflow list and detail pages so the fallback rule is stated
 * once.
 */
export function registrantLabel(v: {
  created_by_label?: string | null;
  created_by?: string | null;
}): string | null | undefined {
  return v.created_by_label ?? v.created_by;
}

/** A version's engine and its pinned release, if any: `snakemake 9.1.0`. */
export const engineLabel = (v: { engine: string; engine_version?: string | null }): string =>
  v.engine_version ? `${v.engine} ${v.engine_version}` : v.engine;

type Groupable = {
  project: string;
  name: string;
  created_at_micros: number;
  run_count?: number | null;
};

/** One workflow — a (project, name) — and its registered versions, newest first. */
export interface VersionGroup<V extends Groupable = WorkflowSummary> {
  /** A key that can't collide even when a name or project contains a separator character. */
  key: string;
  project: string;
  name: string;
  /** The newest version. */
  latest: V;
  /** Every version, newest first (the list's order on a tie). */
  versions: V[];
  /** Runs of any version. */
  runCount: number;
}

/** The key of the workflow `v` is a version of. */
export const workflowKey = (v: { project: string; name: string }): string =>
  JSON.stringify([v.project, v.name]);

/** The pipeline name a `workflowKey` holds (the key itself if it is not one), for a pin no list names. */
export function pinName(key: string): string {
  try {
    const [, name] = JSON.parse(key) as [string, string];
    return typeof name === 'string' ? name : key;
  } catch {
    return key;
  }
}

/**
 * Group registered versions into workflows (one per project and name), in the order each workflow
 * is first seen. "Latest" is the newest registration, whatever order the list came in.
 */
export function groupVersions<V extends Groupable>(list: readonly V[]): VersionGroup<V>[] {
  const map = new Map<string, V[]>();
  for (const v of list) {
    const key = workflowKey(v);
    const versions = map.get(key);
    if (versions) versions.push(v);
    else map.set(key, [v]);
  }
  return [...map].map(([key, found]) => {
    // A stable sort: versions registered in the same microsecond keep the list's order.
    const versions = [...found].sort((a, b) => b.created_at_micros - a.created_at_micros);
    return {
      key,
      project: versions[0].project,
      name: versions[0].name,
      latest: versions[0],
      versions,
      runCount: versions.reduce((n, v) => n + (v.run_count ?? 0), 0),
    };
  });
}
