// How a key result opens: in the FastQC viewer, the genome browser or the
// output viewer, by its format; and what *QC at a glance* offers. Pure, so it is unit-tested. The
// Lab's result cards and the Expert Reports tab use the same rules, so the Lab modules import these
// from here (never the other way round).

import type { RunOutput, RunReport } from '#lib/api/index.js';
import type { ViewerFile } from '#lib/components/viewerFile.js';

/**
 * How Open shows a result: the FastQC viewer (`fastqc`: the pipeline's FastQC result as data), the
 * genome browser (`alignment`: a BAM or CRAM), or the output viewer (`viewer`: anything with a
 * `viewer`).
 */
export type OpenKind = 'fastqc' | 'alignment' | 'viewer';

/** Whether a file is an alignment the genome browser shows (a BAM or CRAM). */
export const isAlignment = (name: string): boolean => /\.(bam|cram)$/i.test(name);

/** Whether a file is a pipeline's FastQC report (its HTML or its zip, with a sample before
 * `_fastqc`, as the server's `report_format` reads it). */
export const isFastqcReport = (name: string): boolean => /.+_fastqc\.(html|zip)$/i.test(name);

/**
 * Whether the browser can have `output` from biffix: a download link is issued for it (`servable`)
 * and it is still stored. Open and Download are offered only then, wherever a result is listed.
 */
export const isServable = (o: Pick<RunOutput, 'servable' | 'exists'>): boolean =>
  o.servable && o.exists !== false;

/** Why an output that is not `isServable` has no Open or Download, in a few words. */
export const unservedText = (o: Pick<RunOutput, 'servable' | 'exists'>): string =>
  o.exists === false ? 'No longer stored' : "Can't be downloaded here";

/** How `output`, a result of `format`, opens (`OpenKind`); null when it does not (download it). */
export function openKind(
  format: string,
  output: Pick<RunOutput, 'name' | 'viewer'>,
): OpenKind | null {
  if (format === 'fastqc') return 'fastqc';
  if (format === 'alignment' && isAlignment(output.name)) return 'alignment';
  return output.viewer ? 'viewer' : null;
}

/** What Open shows: an output in the output viewer, the FastQC viewer or the genome browser. */
export type OpenTarget =
  | { kind: 'viewer'; file: ViewerFile }
  | { kind: 'fastqc' | 'alignment'; runId: string; output: RunOutput; title: string };

/**
 * The target for opening `output` as `kind`; `file` is its output-viewer file (the output viewer's,
 * and the FastQC viewer's fallback to the HTML report).
 */
export function openTarget(
  kind: OpenKind,
  runId: string,
  output: RunOutput,
  file: ViewerFile,
): OpenTarget {
  return kind === 'viewer' ? { kind, file } : { kind, runId, output, title: file.name };
}

/**
 * How a picker names each of `entries`: by its sample, or `sample · file name` for a sample listed
 * more than once (a paired-end sample's R1 and R2); an entry without a sample by its file name.
 */
export function sampleLabels(
  entries: readonly { sample?: string | null; output: Pick<RunOutput, 'name'> }[],
): string[] {
  const count = new Map<string, number>();
  for (const e of entries) if (e.sample) count.set(e.sample, (count.get(e.sample) ?? 0) + 1);
  return entries.map((e) =>
    !e.sample
      ? e.output.name
      : count.get(e.sample) === 1
        ? e.sample
        : `${e.sample} · ${e.output.name}`,
  );
}

/**
 * `reports` without each FastQC zip whose HTML report (`x_fastqc.html` beside `x_fastqc.zip`) is
 * also among them: both open the same FastQC result, so one choice (the HTML's, which also has the
 * full report) is enough. A declaration such as `fastqc/{sample}*` matches both.
 */
export function withoutFastqcTwins<T extends Pick<RunReport, 'format' | 'output'>>(
  reports: readonly T[],
): T[] {
  const fastqcPaths = new Set(
    reports.filter((r) => r.format === 'fastqc').map((r) => r.output.path.toLowerCase()),
  );
  return reports.filter((r) => {
    if (r.format !== 'fastqc') return true;
    const path = r.output.path.toLowerCase();
    return !path.endsWith('_fastqc.zip') || !fastqcPaths.has(path.replace(/\.zip$/, '.html'));
  });
}

/** One choice in *QC at a glance*: a sample (or, without one, the file's name) and its report. */
export interface QcChoice {
  label: string;
  report: RunReport;
}

/**
 * What *QC at a glance* offers beside the General Statistics: each FastQC result (declared or
 * detected, one per result even when its HTML and zip are both listed) and each BAM/CRAM alignment
 * result, in the server's order. A sample with several files (R1 and R2) names each by its file.
 */
export function qcChoices(reports: readonly RunReport[]): {
  fastqc: QcChoice[];
  alignments: QcChoice[];
} {
  const choices = (want: OpenKind): QcChoice[] => {
    const picked = withoutFastqcTwins(
      reports.filter((r) => isServable(r.output) && openKind(r.format, r.output) === want),
    );
    const labels = sampleLabels(picked.map((r) => ({ sample: r.sample, output: r.output })));
    return picked.map((report, i) => ({ label: labels[i], report }));
  };
  return { fastqc: choices('fastqc'), alignments: choices('alignment') };
}
