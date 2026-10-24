// The Data browser's pure parts, shared by the Lab and Expert presentations (`DataBrowser.svelte`):
// a folder's trail in either view, which analyses a listing names, what a file opens as, and what
// opening a file as its analysis's result came to. Pure, so they are unit-tested; the component
// keeps the state, and the words are in `presentation.ts`.

import type { DataFile, DataFolder, DataSource } from '#lib/api/index.js';
import { isAlignment, isFastqcReport, type OpenTarget } from '#lib/qc/open.js';
import { hiddenIn, type HideRule as SourceRule } from './browse';
import { RUNS_FOLDER, runFolderId, runOfPath, showsRuns } from './labPresentation';
import { breadcrumbs } from './paths';
import type { RunOutputLookup } from './runOutput';

type NamedSource = Pick<DataSource, 'name'> & SourceRule;

/** One step of a folder's trail: a link, a folder the source hides (named, never linked), or the
 * folder on show. */
export interface TrailStep {
  label: string;
  path: string;
  kind: 'link' | 'hidden' | 'current';
}

/**
 * The Expert trail to `path`: every folder from the source's root, the last one the folder on show,
 * and a folder the source hides (`biffix/` above a run's folder) named but not linked.
 */
export function expertTrail(source: NamedSource, path: string): TrailStep[] {
  const all = breadcrumbs(source.name, path);
  return all.map((c, i) => ({
    label: c.label,
    path: c.path,
    kind: i === all.length - 1 ? 'current' : hiddenIn(source, c.path) ? 'hidden' : 'link',
  }));
}

/**
 * The Lab trail to `path`: the folders the source hides left out (the project's `biffix/` above its
 * runs, which cannot be opened), the runs' folder called *Analysis results*, and an analysis's
 * folder by its name (`nameOf`, else its id). Every step but the last a link; the last, the folder
 * on show, is there even when the source hides it (a typed link to one).
 */
export function labTrail(
  source: NamedSource,
  path: string,
  nameOf: (runId: string) => string | null,
): TrailStep[] {
  const all = breadcrumbs(source.name, path);
  return all.flatMap((c, i): TrailStep[] => {
    const current = i === all.length - 1;
    if (i > 0 && !current && hiddenIn(source, c.path)) return [];
    const run = runFolderId(source, c.path);
    const label =
      showsRuns(source) && c.path === RUNS_FOLDER
        ? 'Analysis results'
        : run
          ? (nameOf(run) ?? run)
          : c.label;
    return [{ label, path: c.path, kind: current ? 'current' : 'link' }];
  });
}

/**
 * The analyses a listing of `path` names: the one whose results folder it is, those whose folders
 * it lists, and those the search results are in.
 */
export function analysesShown(
  source: SourceRule,
  path: string,
  folders: readonly Pick<DataFolder, 'path'>[],
  searchResults: readonly Pick<DataFile, 'path'>[] | null,
): Set<string> {
  const ids = new Set<string>();
  const run = runOfPath(source, path);
  if (run) ids.add(run.runId);
  for (const f of folders) {
    const id = runFolderId(source, f.path);
    if (id) ids.add(id);
  }
  for (const f of searchResults ?? []) {
    const r = runOfPath(source, f.path);
    if (r) ids.add(r.runId);
  }
  return ids;
}

/**
 * The analysis a file listed in `path` is a result of, when that is another analysis than the one
 * whose folder `path` is (a search result from another run's folder); else null.
 */
export function resultOfOther(source: SourceRule, path: string, filePath: string): string | null {
  const r = runOfPath(source, filePath);
  return r && r.runId !== runOfPath(source, path)?.runId ? r.runId : null;
}

/**
 * What a file opens as besides the file viewer: its analysis's alignment or FastQC report (only in
 * an analysis's results folder), or nothing.
 */
export function opensAs(
  source: SourceRule,
  f: Pick<DataFile, 'name' | 'path'>,
): 'alignment' | 'fastqc' | null {
  if (!runOfPath(source, f.path)) return null;
  if (isAlignment(f.name)) return 'alignment';
  if (isFastqcReport(f.name)) return 'fastqc';
  return null;
}

/**
 * What opening a file as its analysis's result came to: its viewer, the file viewer, or why not
 * (its analysis didn't record it, or biffix couldn't find it among the analysis's results).
 */
export type ResultOpen =
  { open: OpenTarget } | { inViewer: true } | { cannot: 'notRecorded' | 'notFound' };

/**
 * What to do with the lookup `found` of file `f` (of run `runId`) opened as `kind`: open the
 * output; open a FastQC report its analysis did not record (one a resumed run wrote into the shared
 * folder, say) in the file viewer, as the Expert view does; or why it can't be opened.
 */
export function resultOpen(
  found: RunOutputLookup,
  kind: 'alignment' | 'fastqc',
  runId: string,
  f: Pick<DataFile, 'name' | 'viewer'>,
): ResultOpen {
  if ('output' in found) return { open: { kind, runId, output: found.output, title: f.name } };
  if (kind === 'fastqc' && f.viewer) return { inViewer: true };
  return { cannot: 'notRecorded' in found ? 'notRecorded' : 'notFound' };
}
