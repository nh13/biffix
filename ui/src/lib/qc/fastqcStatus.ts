// FastQC from MultiQC: for a run whose pipeline published MultiQC's output but not
// FastQC's own zips, each FastQC result's module statuses and basic statistics as MultiQC recorded
// them (`GET /runs/{id}/qc/fastqc-status`). Statuses only: MultiQC's data has no FastQC plots.
// Pure, so it is unit-tested; the component only draws what these return.

import type { FastqcStatusSample } from '$lib/api';
import { sampleLabels } from './open';

/** One choice in the picker: how it reads and the result. */
export interface StatusChoice {
  label: string;
  result: FastqcStatusSample;
}

/**
 * The picker's choices, in the server's order (the run's samples first): by sample, or
 * `sample · MultiQC name` for a sample with several results (a paired-end sample's R1 and R2); a
 * result no sample owns by its MultiQC name.
 */
export function statusChoices(samples: readonly FastqcStatusSample[]): StatusChoice[] {
  const labels = sampleLabels(
    samples.map((s) => ({ sample: s.sample, output: { name: s.fastqc_name } })),
  );
  return samples.map((result, i) => ({ label: labels[i], result }));
}

/**
 * Where `from[at]` is among `to` (another section's choices), so a section change keeps the same
 * result: the same MultiQC name of the same sample, else the same sample's file at the same place
 * among that sample's files (trimming renames them), else the first choice. Never by index alone:
 * a section can lack a result (a sample trimming left with no reads), shifting the rest.
 */
export function samePick(
  from: readonly StatusChoice[],
  at: number,
  to: readonly StatusChoice[],
): number {
  const was = from[at]?.result;
  if (!was) return 0;
  const exact = to.findIndex(
    (c) => c.result.sample === was.sample && c.result.fastqc_name === was.fastqc_name,
  );
  if (exact >= 0) return exact;
  if (was.sample == null) return 0;
  const place = from.slice(0, at).filter((c) => c.result.sample === was.sample).length;
  const same = to.flatMap((c, i) => (c.result.sample === was.sample ? [i] : []));
  return same[place] ?? 0;
}

/** One FastQC section of the MultiQC report (FastQC run on raw reads, say) and its choices. */
export interface StatusSection {
  /** The section's anchor (`fastqc_trimmed`); `null` for the unanchored one. */
  section: string | null;
  /** The anchor as written; MultiQC's own anchor, `fastqc`, for the unanchored section. */
  label: string;
  choices: StatusChoice[];
}

/**
 * The report's FastQC sections in the server's order (the unanchored one first), each with its
 * results' picker choices ([`statusChoices`]). One section for a report that ran FastQC once; raw
 * and trimmed reads are two, and may hold the same MultiQC names.
 */
export function statusSections(samples: readonly FastqcStatusSample[]): StatusSection[] {
  const order: (string | null)[] = [];
  for (const s of samples) {
    const section = s.section ?? null;
    if (!order.includes(section)) order.push(section);
  }
  return order.map((section) => ({
    section,
    label: section ?? 'fastqc',
    choices: statusChoices(samples.filter((s) => (s.section ?? null) === section)),
  }));
}

/** A number with at most `decimals` decimals, without trailing zeros (`50.0` → `50`). */
function trim(value: number, decimals: number): string {
  return value.toLocaleString('en-US', { maximumFractionDigits: decimals });
}

/** The basic statistics MultiQC recorded, as `[label, value]` rows; one it lacks is left out. */
export function basicStatRows(stats: FastqcStatusSample['stats']): [string, string][] {
  const rows: [string, string | null][] = [
    [
      'Total sequences',
      stats.total_sequences != null ? stats.total_sequences.toLocaleString('en-US') : null,
    ],
    ['Total bases', stats.total_bases ?? null],
    ['Sequence length', stats.sequence_length != null ? `${stats.sequence_length} bp` : null],
    ['GC', stats.percent_gc != null ? `${trim(stats.percent_gc, 1)}%` : null],
    [
      'Duplicates',
      stats.percent_duplicates != null ? `${trim(stats.percent_duplicates, 1)}%` : null,
    ],
    [
      'Flagged poor quality',
      stats.poor_quality != null ? stats.poor_quality.toLocaleString('en-US') : null,
    ],
  ];
  return rows.filter((r): r is [string, string] => r[1] !== null);
}
