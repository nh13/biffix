// Samples detected from picked files, as the builder previews them before anything is added: the
// preview's one-line summary, the sample id the naming rule reads off the example file, the parts
// of its name chosen as the id, and the merges the server suggests that still apply.

import type { DetectedSamples } from '$lib/api';
import type { DetectRule, MergeSuggestion } from '$lib/generated/samples';
import { plural } from '$lib/util/format';
import type { SheetRow } from './sheet';

/** The rule a fresh pick is detected with: ids read from file names, the common schemes. */
export function autoRule(): DetectRule {
  return { id_from: 'name', method: { kind: 'auto' } };
}

/** The parts of the example name `rule` takes as the id (none unless it chooses parts). */
export function selectedTokens(rule: DetectRule): number[] {
  return rule.method?.kind === 'tokens' ? rule.method.selected : [];
}

/** `selected` with part `i` chosen, or no longer chosen, in name order. */
export function toggleToken(selected: readonly number[], i: number): number[] {
  const set = new Set(selected);
  if (set.has(i)) set.delete(i);
  else set.add(i);
  return [...set].sort((a, b) => a - b);
}

/**
 * The id of the sample the example file landed in, or null when the rule read none from it.
 * `exampleUri` is the file the server took the example from (the first picked): by its whole URI,
 * since files in different folders often share a base name.
 */
export function exampleSampleId(
  preview: DetectedSamples,
  exampleUri: string | undefined,
): string | null {
  if (!preview.example || !exampleUri) return null;
  const s = preview.detection.samples.find((x) => [...x.read1, ...x.read2].includes(exampleUri));
  return s?.id ?? null;
}

/** The preview's summary: samples, pairs, problems, and the files not matched or left out. */
export function detectionSummary(preview: DetectedSamples): string {
  const samples = preview.detection.samples;
  const paired = samples.filter((s) => s.read1.length > 0 && s.read2.length > 0).length;
  const flagged = samples.filter((s) => (s.problems?.length ?? 0) > 0).length;
  const unmatched = preview.detection.unmatched?.length ?? 0;
  // Index reads, repeats and uncompressed copies of gzipped files, left out on purpose.
  const skipped = preview.detection.skipped?.length ?? 0;
  return [
    plural(samples.length, 'sample'),
    `${paired} paired`,
    `${flagged} with problems`,
    unmatched ? `${unmatched} files not matched` : '',
    skipped ? `${plural(skipped, 'file')} left out` : '',
  ]
    .filter(Boolean)
    .join(' · ');
}

/**
 * The suggestions that still stand: those whose two samples are both still in `rows` (Clear, an
 * import, a removal or an edited id can take them away). No id column: none.
 */
export function standingSuggestions<S extends Pick<MergeSuggestion, 'ids'>>(
  suggestions: readonly S[],
  rows: readonly SheetRow[],
  idColumn: string | null,
): S[] {
  if (!idColumn) return [];
  const ids = new Set(rows.map((r) => r[idColumn]));
  return suggestions.filter((s) => s.ids.every((id) => ids.has(id)));
}

/** A URI's last path part (the file's name). */
export function baseName(uri: string): string {
  return uri.split('/').pop() ?? uri;
}
