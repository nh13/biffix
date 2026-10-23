// Pure helpers for what the run page shows about a run.

import type { RunDetail } from '#lib/api/index.js';
import { priceSourceLabel } from '#lib/runs/attempts.js';
import { plural } from '#lib/util/format.js';

/** `v` when it is a plain object (a run's params, its input tables), else an empty one. */
export const asRecord = (v: unknown): Record<string, unknown> =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {};

/** A run's resolved params (already secret-redacted server-side) as [key, value], sorted by key. */
export function paramEntries(params: RunDetail['params'] | undefined): [string, unknown][] {
  return Object.entries(asRecord(params)).sort(([a], [b]) => a.localeCompare(b));
}

/** The cost estimate's basis, for its tooltip: its price sources and what it leaves out. */
export function costBasis(cost: RunDetail['cost'] | undefined | null): string {
  if (!cost) return '';
  return [
    `Estimated from ${plural(cost.priced_jobs, 'job')}: ` +
      cost.price_sources.map(priceSourceLabel).join('; ') +
      '.',
    cost.pending_jobs > 0 ? `${plural(cost.pending_jobs, 'job')} not priced yet.` : '',
    'Compute + root volume for the time each job ran; excludes host boot/idle, credits and Savings Plans. Not the bill.',
  ]
    .filter(Boolean)
    .join(' ');
}

/**
 * The run's image map for display: each tenant image its tasks could run, with the declared
 * references that resolve to it. Leaves out identity entries (an image mapped to itself) and
 * biffix's own keys (`$default`), which say nothing a reader needs. Sorted by first reference.
 */
export function imageMapRows(
  map: Record<string, string> | null | undefined,
): { image: string; refs: string[] }[] {
  const byImage = new Map<string, string[]>();
  for (const [ref, image] of Object.entries(map ?? {})) {
    if (ref === image || ref.startsWith('$')) continue;
    byImage.set(image, [...(byImage.get(image) ?? []), ref]);
  }
  return [...byImage]
    .map(([image, refs]) => ({ image, refs: refs.sort() }))
    .sort((a, b) => a.refs[0].localeCompare(b.refs[0]));
}

/**
 * The images the run was submitted with (`--image`), as rows: each reference as given, and what it
 * was pinned to at submit. Sorted by reference; empty when the submit named none.
 */
export function submitImageRows(
  images: { declared_ref: string; source_ref: string }[] | null | undefined,
): { declared: string; pinned: string }[] {
  // Each spelling once (the server records each once; this keeps a keyed list safe regardless).
  const bySpelling = new Map<string, string>();
  for (const i of images ?? [])
    if (!bySpelling.has(i.declared_ref)) bySpelling.set(i.declared_ref, i.source_ref);
  return [...bySpelling]
    .map(([declared, pinned]) => ({ declared, pinned }))
    .sort((a, b) => a.declared.localeCompare(b.declared));
}
