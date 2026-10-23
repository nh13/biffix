// Whether a workflow version can run, as the page shows it: the version's state (its engine
// runtime and every image must be built first), a chip for the header, and labels for each image.
// The server decides; this only turns its `state` / `not_ready` / `images[].state` into words.

/**
 * `unknown`: the server reports no state (it could not read the version's readiness just now), so
 * the launch form is offered and submit decides.
 * `other`: the server reports a state this UI doesn't know (the enum is open). That is not an
 * absence of state, so it is not assumed runnable: the server may be saying it can't run.
 */
export type VersionState = 'ready' | 'pending' | 'failed' | 'unknown' | 'other';

export interface Readiness {
  state: VersionState;
  /** The server's raw state, for one this UI doesn't know (`other`). */
  label?: string;
  /** Why it cannot run now (empty when ready). */
  reasons: string[];
}

interface Detailish {
  state?: string | null;
  not_ready?: string[] | null;
}

/** A version's readiness. No state (the server could not read it): treat as runnable. */
export function readinessOf(detail: Detailish): Readiness {
  const s = detail.state;
  if (s == null) return { state: 'unknown', reasons: [] };
  if (s === 'ready') return { state: s, reasons: [] };
  const reasons = detail.not_ready ?? [];
  if (s === 'pending' || s === 'failed') return { state: s, reasons };
  return { state: 'other', label: s, reasons };
}

/** Whether the launch form can be offered. */
export function canRun(r: Readiness): boolean {
  return r.state === 'ready' || r.state === 'unknown';
}

/** The header chip for a version that cannot run yet; none when it can. */
export function stateChip(r: Readiness): { label: string; cls: string } | null {
  if (r.state === 'pending') return { label: 'building…', cls: 'building' };
  if (r.state === 'failed') return { label: 'build failed', cls: 'failed' };
  if (r.state === 'other') return { label: r.label ?? 'not ready', cls: 'other' };
  return null;
}

/** An image's import (or, for one built from a recipe, build) state in words. */
export function imageStateLabel(state: string, built = false): string {
  switch (state) {
    case 'ready':
      return built ? 'built' : 'imported';
    case 'building':
      return built ? 'building…' : 'importing…';
    case 'unbuilt':
      return 'queued';
    case 'failed':
      return 'failed';
    default:
      return state;
  }
}

interface ImageStateish {
  state: string;
  /** `import`, or how it is built (`dockerfile`, `pixi`). */
  kind?: string | null;
  /** The imported image, once imported. */
  image?: string | null;
  /** A Snakemake image's derive (the engine runtime added), once imported. */
  derived_state?: string | null;
}

/** An image's state in words: its import, then (a Snakemake image) the runtime being added. */
export function imageLabel(i: ImageStateish): string {
  if (i.derived_state && i.image) {
    if (i.state === 'ready') return 'ready';
    if (i.state !== 'failed') return 'adding runtime…';
  }
  return imageStateLabel(i.state, isBuilt(i));
}

/** Whether an image is built from a recipe in the workflow (not imported). */
export function isBuilt(i: { kind?: string | null }): boolean {
  return !!i.kind && i.kind !== 'import';
}

/** A built image's per-architecture build state as a short mark. */
export function archMark(state: string): string {
  switch (state) {
    case 'succeeded':
      return '✓';
    case 'failed':
      return '✗';
    case 'unbuilt':
      return '·';
    default:
      return '…';
  }
}

/** The part of a digest worth showing (`sha256:` + 12 hex). */
export function shortDigest(ref: string | null | undefined): string {
  if (!ref) return '--';
  const d = ref.includes('@') ? ref.slice(ref.lastIndexOf('@') + 1) : ref;
  return d.startsWith('sha256:') ? d.slice(0, 19) : d;
}
