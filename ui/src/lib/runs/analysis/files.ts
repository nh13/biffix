// The Lab view's file list for an analysis (*All files*): every file it wrote, by
// what it is — the key results the pipeline declares first, then its other results, then its logs
// and measurements — with plain names, no task or role columns. Pure, so it is unit-tested.

import type { RunOutput, RunReport } from '$lib/api';
import { folderOf } from '$lib/data/paths';
import { isAlignment, isFastqcReport, isServable, openKind, type OpenKind } from '$lib/qc/open';

/** One file as the list shows it. */
export interface LabFile {
  output: RunOutput;
  /** The key result it is, when the pipeline declares (or biffix finds) it as one. */
  result: { name: string; sample: string | null; format: string } | null;
  /** Its folder under the analysis's results folder (`''` at the top, else ending in `/`). */
  folder: string;
}

export type FileGroupId = 'results' | 'other' | 'logs';

export interface FileGroup {
  id: FileGroupId;
  title: string;
  /** One line on what the group holds. */
  note: string;
  files: LabFile[];
}

/** Roles that are a run's records rather than its results (`RunOutput.role`, an open set). */
const RECORD_ROLES = new Set(['log', 'benchmark']);

const fileOf = (output: RunOutput, result: LabFile['result'] = null): LabFile => ({
  output,
  result,
  folder: folderOf(output.path),
});

/**
 * The groups for an analysis's `outputs` and its key results (`reports`, in the server's order):
 * *Key results* (each report's file once, in report order, a FastQC zip beside its HTML report
 * included: both are files), *Other results*, and *Logs and measurements* (logs and benchmarks),
 * each of the last two by path. Work-directory intermediates are never listed. Empty groups are
 * left out.
 */
export function groupFiles(
  outputs: readonly RunOutput[],
  reports: readonly RunReport[],
): FileGroup[] {
  const results: LabFile[] = [];
  const seen = new Set<string>();
  for (const r of reports) {
    if (seen.has(r.output.id) || r.output.role === 'intermediate') continue;
    seen.add(r.output.id);
    results.push(fileOf(r.output, { name: r.name, sample: r.sample ?? null, format: r.format }));
  }
  const other: LabFile[] = [];
  const logs: LabFile[] = [];
  for (const o of outputs) {
    if (seen.has(o.id) || o.role === 'intermediate') continue;
    seen.add(o.id);
    (RECORD_ROLES.has(o.role) ? logs : other).push(fileOf(o));
  }
  const byPath = (a: LabFile, b: LabFile) => a.output.path.localeCompare(b.output.path);
  other.sort(byPath);
  logs.sort(byPath);
  const groups: FileGroup[] = [
    {
      id: 'results',
      title: 'Key results',
      note: 'The results this pipeline points out.',
      files: results,
    },
    {
      id: 'other',
      title: results.length ? 'Other results' : 'Results',
      note: 'Everything else the analysis wrote.',
      files: other,
    },
    {
      id: 'logs',
      title: 'Logs and measurements',
      note: 'What each step printed and how long it took: for troubleshooting.',
      files: logs,
    },
  ];
  return groups.filter((g) => g.files.length > 0);
}

/** Whether `file` matches a search: its name, folder, result name or sample, ignoring case. */
export function matchesFile(file: LabFile, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [file.output.name, file.folder, file.result?.name, file.result?.sample].some((s) =>
    (s ?? '').toLowerCase().includes(q),
  );
}

/** `groups` with only the files matching `query`; groups left empty are dropped. */
export function filterGroups(groups: readonly FileGroup[], query: string): FileGroup[] {
  if (!query.trim()) return [...groups];
  return groups
    .map((g) => ({ ...g, files: g.files.filter((f) => matchesFile(f, query)) }))
    .filter((g) => g.files.length > 0);
}

/**
 * How Open shows a file, or `null` (download it): a key result by its format, any other file by
 * its name (a pipeline's FastQC report in the FastQC viewer, a BAM or CRAM in the genome browser)
 * or by its viewer. Never a file biffix does not serve or that is no longer stored.
 */
export function fileOpenKind(file: LabFile): OpenKind | null {
  const o = file.output;
  if (!isServable(o)) return null;
  if (file.result) return openKind(file.result.format, o);
  if (isFastqcReport(o.name)) return 'fastqc';
  return openKind(isAlignment(o.name) ? 'alignment' : 'file', o);
}

/** The label a file is shown under: its result's name (and sample), else its own name. */
export function fileTitle(file: LabFile): string {
  if (!file.result) return file.output.name;
  return file.result.sample ? `${file.result.name} · ${file.result.sample}` : file.result.name;
}

/** How many of a group's files are shown at first, and added by each *Show more*. */
export const FILES_PER_PAGE = 50;

/** The files of a group shown after `pages` pages, and how many are left. */
export function shownFiles<T>(
  files: readonly T[],
  pages: number,
  perPage = FILES_PER_PAGE,
): { shown: T[]; left: number } {
  const n = Math.max(1, pages) * perPage;
  return { shown: files.slice(0, n), left: Math.max(0, files.length - n) };
}
