// An experiment's results across its analyses, beyond one wall of cards per
// analysis: the same key result of every analysis side by side, grouped by its kind (reports,
// tables, images, per-sample QC, alignments…), as a compact table (analyses × results) or a card
// per analysis under each result; and one sample's results across them all. Built on the analysis
// page's cards (`resultCards`), so a result opens and downloads the same way. Pure, so unit-tested.

import type { ResultCard } from '$lib/runs/results';
import { formatLabel } from '$lib/runs/results';

/** How the experiment page shows results across analyses. */
export type ResultsView = 'table' | 'result' | 'analysis';

export const RESULTS_VIEWS: { id: ResultsView; label: string }[] = [
  { id: 'table', label: 'Table' },
  { id: 'result', label: 'By result' },
  { id: 'analysis', label: 'By analysis' },
];

/** The view to start in: the remembered one, else a table when the experiment has two or more
 * analyses (`analyses` counts those that may have results, before any have loaded). */
export function initialView(saved: string | null, analyses: number): ResultsView {
  if (saved === 'table' || saved === 'result' || saved === 'analysis') return saved;
  return analyses > 1 ? 'table' : 'analysis';
}

/** One analysis's cards, for the views across analyses. */
export interface RunCards {
  runId: string;
  cards: ResultCard[];
}

/** A result some analysis has: its name, how it shows, whether it is one per sample. */
export interface ResultColumn {
  name: string;
  format: string;
  perSample: boolean;
}

/** Results of one kind, under the kind's label (`Report`, `Table`, `FastQC`…). */
export interface KindGroup {
  format: string;
  label: string;
  columns: ResultColumn[];
}

/** The order kinds read in: what an analysis is for first, then QC, then the raw files. */
const KIND_ORDER = ['html', 'pdf', 'table', 'image', 'fastqc', 'alignment', 'reads', 'file'];
const kindRank = (format: string) => {
  const i = KIND_ORDER.indexOf(format);
  return i < 0 ? KIND_ORDER.length : i;
};

/** A card's format: its first entry's (an author's `kind`, else the server's guess per file). */
export const cardFormat = (card: ResultCard): string => card.entries[0]?.report.format ?? 'file';

/**
 * Every result any analysis has, once by name (a name stands for all of an analysis's cards of it:
 * a glob's files, a per-sample result split over two declarations; its kind is the first's),
 * grouped by kind in `KIND_ORDER`, each group's results in the order they first appear.
 */
export function kindGroups(runs: readonly RunCards[]): KindGroup[] {
  const columns = new Map<string, ResultColumn>();
  for (const run of runs)
    for (const card of run.cards)
      if (!columns.has(card.name))
        columns.set(card.name, {
          name: card.name,
          format: cardFormat(card),
          perSample: card.perSample,
        });
  const groups = new Map<string, KindGroup>();
  for (const column of columns.values()) {
    const label = formatLabel(column.format);
    let group = groups.get(label);
    if (!group) {
      group = { format: column.format, label, columns: [] };
      groups.set(label, group);
    }
    group.columns.push(column);
  }
  return [...groups.values()].sort((a, b) => kindRank(a.format) - kindRank(b.format));
}

/**
 * The analysis's result of a name, as one card holding every card's entries of that name (the
 * first's key and description), filtered to `sample`; null if none.
 */
export function cardFor(
  cards: readonly ResultCard[],
  name: string,
  sample: string | null,
): ResultCard | null {
  const named = cards.filter((c) => c.name === name);
  if (named.length === 0) return null;
  const card =
    named.length === 1
      ? named[0]
      : {
          ...named[0],
          perSample: named.some((c) => c.perSample),
          entries: named.flatMap((c) => c.entries),
        };
  return forSample(card, sample);
}

/**
 * `card` for one sample: a per-sample card keeps only that sample's files (null when the sample has
 * none in it); a whole-study result is the same for every sample, so it is kept as it is.
 */
export function forSample(card: ResultCard, sample: string | null): ResultCard | null {
  if (!sample || !card.perSample) return card;
  const entries = card.entries.filter((e) => e.sample === sample);
  return entries.length ? { ...card, entries } : null;
}

/** Every sample any analysis's per-sample results name, in natural order. */
export function resultSamples(runs: readonly RunCards[]): string[] {
  const seen = new Set<string>();
  for (const run of runs)
    for (const card of run.cards)
      if (card.perSample) for (const e of card.entries) if (e.sample) seen.add(e.sample);
  return [...seen].sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
}
