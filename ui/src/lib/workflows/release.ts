// Released vs developer versions. A Developer or Admin releases a specific version; every other
// version is a developer version. The Workflows pages show, by default, only pipelines with a
// released version (the Developer filter shows them all), and a launch from the catalog takes a
// pipeline's newest released version.

type Releasable = { release?: { released?: boolean | null } | null };

/** Whether a version is released (anything but an explicit `true`: a developer version). */
export const isReleased = (v: Releasable): boolean => v.release?.released === true;

/** Whether any version of a pipeline is released. */
export const hasRelease = <V extends Releasable>(g: { versions: readonly V[] }): boolean =>
  g.versions.some(isReleased);

/**
 * The version a launch from the catalog takes: the newest released one, else (nothing released)
 * the newest, a developer version. `versions` is newest first, as `groupVersions` gives them.
 */
export function launchVersion<V extends Releasable>(g: { latest: V; versions: readonly V[] }): V {
  return g.versions.find(isReleased) ?? g.latest;
}

/** A pipeline's friendly title (that of the version a launch takes), else its name: the name the
 * Lab's catalog shows it by. */
export const catalogTitle = <V extends Releasable & { title?: string | null }>(g: {
  name: string;
  latest: V;
  versions: readonly V[];
}): string => launchVersion(g).title?.trim() || g.name;

/**
 * The pipelines a Workflows page shows: those with a released version, or (Developer on) all of
 * them. `hiddenByFilter` counts what the filter left out, for the page to say so.
 */
export function releaseFilter<G extends { versions: readonly Releasable[] }>(
  groups: readonly G[],
  developer: boolean,
): { shown: G[]; hiddenByFilter: number } {
  const shown = developer ? groups.slice() : groups.filter(hasRelease);
  return { shown, hiddenByFilter: groups.length - shown.length };
}

/** A version's kind, as its badge says it. */
export const releaseLabel = (v: Releasable): 'Released' | 'Developer' =>
  isReleased(v) ? 'Released' : 'Developer';
