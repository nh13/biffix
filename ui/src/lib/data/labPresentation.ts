// The Lab view's Data page: where a folder came from (an analysis's results folder, named as the
// analysis), a way into the analyses' results from the project's top folder (which hides biffix's
// own folder), and where uploads go. Pure, so it is unit-tested; the Data page reads the source's
// hide rule off the source, never a copy.

import type { DataSource } from '#lib/api/index.js';
import { UPLOADS_FOLDER } from '#lib/upload/keys.js';
import type { HideRule as SourceRule } from './browse';

/** Where a project bucket keeps its runs' folders (`s3_layout::RUNS_PREFIX`). */
export const RUNS_FOLDER = 'biffix/runs/';

/** Whether `source` shows its runs' folders (the project bucket does, as a browsable exception). */
export function showsRuns(source: SourceRule): boolean {
  return (source.browsable_exceptions ?? []).includes(RUNS_FOLDER);
}

/**
 * The run whose results folder `path` (relative to `source`'s root) is in, and the path under that
 * folder: `biffix/runs/<id>/…` of a source that shows its runs' folders. Null anywhere else.
 */
export function runOfPath(
  source: SourceRule,
  path: string,
): { runId: string; rest: string } | null {
  if (!showsRuns(source) || !path.startsWith(RUNS_FOLDER)) return null;
  const tail = path.slice(RUNS_FOLDER.length);
  const cut = tail.indexOf('/');
  if (cut <= 0) return null;
  return { runId: tail.slice(0, cut), rest: tail.slice(cut + 1) };
}

/** The run a folder is the results folder of (`biffix/runs/<id>/` exactly), else null. */
export function runFolderId(source: SourceRule, folderPath: string): string | null {
  const run = runOfPath(source, folderPath);
  return run && run.rest === '' ? run.runId : null;
}

/** A folder the Lab view adds to a source's top folder, for one its listing leaves out. */
export interface Shortcut {
  label: string;
  path: string;
  note: string;
}

/**
 * The folders the Lab view offers at the top of `source` that its listing hides there: the
 * analyses' results (`biffix/runs/`), in the project bucket. None below the top.
 */
export function topShortcuts(source: SourceRule, path: string): Shortcut[] {
  if (path !== '' || !showsRuns(source)) return [];
  return [
    {
      label: 'Analysis results',
      path: RUNS_FOLDER,
      note: 'The files each analysis wrote, a folder per analysis',
    },
  ];
}

/** Whether `source` takes uploads into `path`: a writable source, at its top or under `uploads/`. */
export function uploadsHere(source: Pick<DataSource, 'writable'>, path: string): boolean {
  return source.writable && (path === '' || path.startsWith(UPLOADS_FOLDER));
}

/** How the Lab view names a data source's kind, beside its name (nothing for a plain one). */
export function sourceKindLabel(kind: string): string | null {
  if (kind === 'project') return 'your project';
  if (kind === 'references') return 'genomes';
  return null;
}
