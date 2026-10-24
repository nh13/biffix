// The Lab analysis page's Results: one card per key result the pipeline
// declares, a per-sample result (its files for each sample, sharing a name) folded into one card
// with a sample picker. Pure, so it is unit-tested. How a result opens is `#lib/qc/open`'s.

import type { RunReport } from '#lib/api/index.js';
import { withoutFastqcTwins } from '#lib/qc/open.js';
import { plural } from '#lib/util/format.js';

/** One card: a report, or a per-sample report's entries (each sample's files). */
export interface ResultCard {
  /** Stable across polls: the name (and its declaration's place) for a per-sample card, else the
   * report's output. */
  key: string;
  name: string;
  description: string | null;
  /**
   * Its reports: one, or each sample's files in the server's order (the samplesheet's; a sample's
   * several files, R1 and R2, together in path order).
   */
  entries: { sample: string | null; report: RunReport }[];
  /** Whether it is a per-sample result (with a sample picker). */
  perSample: boolean;
}

/** A name's latest per-sample card, and how many cards the name has. */
interface Group {
  card: ResultCard;
  count: number;
}

/**
 * Whether `report` (which names a sample) belongs to `group`'s card: it came from the same
 * declaration (`declaration`; none for the FastQC results found by name, which are one card).
 */
function continuesCard(group: Group, report: RunReport): boolean {
  const last = group.card.entries[group.card.entries.length - 1].report;
  return (last.declaration ?? null) === (report.declaration ?? null);
}

/**
 * The cards for `reports`, in the server's order (declared first, then MultiQC, then FastQC results
 * found by name): each report its own card, except reports that name a sample, which fold into one
 * card per declaration (see `continuesCard`), keeping the server's order. A FastQC zip listed
 * beside its HTML report is left out of the card: both open the same result.
 */
export function resultCards(reports: readonly RunReport[]): ResultCard[] {
  const cards: ResultCard[] = [];
  const latest = new Map<string, Group>();
  for (const report of reports) {
    const sample = report.sample;
    if (sample) {
      let group = latest.get(report.name);
      if (!group || !continuesCard(group, report)) {
        const count = (group?.count ?? 0) + 1;
        const card: ResultCard = {
          key: count === 1 ? `name:${report.name}` : `name:${report.name}:${count}`,
          name: report.name,
          description: report.description ?? null,
          entries: [],
          perSample: true,
        };
        group = { card, count };
        latest.set(report.name, group);
        cards.push(card);
      }
      group.card.entries.push({ sample, report });
    } else {
      cards.push({
        key: `output:${report.output.id}`,
        name: report.name,
        description: report.description ?? null,
        entries: [{ sample: null, report }],
        perSample: false,
      });
    }
  }
  for (const card of cards) {
    if (!card.perSample) continue;
    const kept = new Set(withoutFastqcTwins(card.entries.map((e) => e.report)));
    card.entries = card.entries.filter((e) => kept.has(e.report));
  }
  return cards;
}

/** How many samples a card's entries are (a paired-end sample's two files are one). */
export function sampleCount(card: Pick<ResultCard, 'entries'>): number {
  return new Set(card.entries.map((e) => e.sample)).size;
}

/** "4 samples", "1 sample". */
export const samplesText = (n: number): string => plural(n, 'sample');

const FORMATS: Record<string, string> = {
  html: 'Report',
  table: 'Table',
  image: 'Image',
  pdf: 'PDF',
  alignment: 'Alignments',
  reads: 'Reads',
  fastqc: 'FastQC',
  file: 'File',
};

/** A card's small format label; an unknown format reads as a file. */
export function formatLabel(format: string): string {
  return Object.hasOwn(FORMATS, format) ? FORMATS[format] : FORMATS.file;
}

/** How the card previews a result: the first rows of a table, an image as itself, else nothing. */
export function previewKind(format: string): 'table' | 'image' | null {
  return format === 'table' || format === 'image' ? format : null;
}

/**
 * Why an ended analysis has no key result to show: its version declares none (`undeclared`), it
 * declares some but the analysis wrote none of them (`unwritten`), or the version is not loaded so
 * it cannot be told (`unknown`). Read off the version's frozen form (`form_schema.reports`).
 */
export type NoResults = 'undeclared' | 'unwritten' | 'unknown';

export function noResultsReason(version: { form_schema: unknown } | null): NoResults {
  if (!version) return 'unknown';
  const reports = (version.form_schema as { reports?: unknown } | null)?.reports;
  return Array.isArray(reports) && reports.length > 0 ? 'unwritten' : 'undeclared';
}

/**
 * Whether the hint on declaring key results (`reports:` in `biffix.yaml`) is for this viewer: the
 * version's registrar, who can act on it. When that cannot be told (no signed-in id, or a version
 * registered before registrars were recorded), only someone in the Expert view sees it.
 */
export function showsAuthorHint(
  registrar: string | null | undefined,
  viewer: string | null,
  mode: 'lab' | 'expert',
): boolean {
  return registrar && viewer ? registrar === viewer : mode === 'expert';
}
