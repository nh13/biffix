// Experiments: named groups of a project's analyses, read together. The pure parts
// of the pages — name checks, the samples × analyses table, the one total cost, the words — kept
// apart from rendering so they are unit-tested.

import type { Experiment, ExperimentDetail, RunSummary } from '$lib/api';
import { fmtRelative, fmtUsd, plural } from '$lib/util/format';
import { runName } from '$lib/runs/runName';
import type { ViewMode } from '$lib/view/viewRoutes';
import { t } from '$lib/view/vocab';

/** The longest name the server takes (after trimming), in characters (code points, as the server
 * counts them, so an emoji is one). */
export const MAX_EXPERIMENT_NAME = 200;

/** The name inputs' `maxlength`, in UTF-16 units as the browser counts: room for a name of
 * astral characters (two units each) at the limit, and for a few too many, which the check
 * reports. */
export const NAME_INPUT_MAXLENGTH = MAX_EXPERIMENT_NAME * 2 + 50;

/** The fields of an experiment a name clash is judged on. */
type NameHolder = Pick<Experiment, 'id' | 'name' | 'project' | 'archived_at'>;

/**
 * What is wrong with `name` for an experiment of `project`, or null when the server should take it:
 * blank, too long, or (ignoring case and the spaces either side) the name of another experiment in
 * that project that is not archived. `self` is the experiment being renamed, which never clashes
 * with itself.
 */
export function nameProblem(
  name: string,
  project: string,
  existing: readonly NameHolder[],
  self: string | null = null,
): string | null {
  const trimmed = name.trim();
  if (!trimmed) return 'Give the experiment a name.';
  const length = [...trimmed].length;
  if (length > MAX_EXPERIMENT_NAME)
    return `Keep the name to ${MAX_EXPERIMENT_NAME} characters (it has ${length}).`;
  const key = trimmed.toLowerCase();
  const clash = existing.find(
    (e) =>
      e.id !== self &&
      e.project === project &&
      e.archived_at == null &&
      e.name.trim().toLowerCase() === key,
  );
  return clash ? `“${clash.name}” is already an experiment in project ${project}.` : null;
}

/** The projects a new experiment can live in: those of the registered pipelines and of the
 * experiments already made, sorted, each once. */
export function experimentProjects(
  pipelineProjects: readonly string[],
  experiments: readonly Pick<Experiment, 'project'>[],
): string[] {
  return [...new Set([...pipelineProjects, ...experiments.map((e) => e.project)])]
    .filter(Boolean)
    .sort((a, b) => a.localeCompare(b));
}

/** "3 analyses", "1 run". */
export function runCountText(n: number, mode: ViewMode): string {
  return plural(n, t('runLower', mode), t('runsLower', mode));
}

/** "1 needs a look", "2 need a look"; empty when none of the caller's analyses in it does. */
export function attentionText(e: Pick<Experiment, 'needs_attention'>): string {
  const n = e.needs_attention ?? 0;
  return n > 0 ? `${n} ${n === 1 ? 'needs' : 'need'} a look` : '';
}

/** An experiment's card line: how many analyses, and when the newest started (else when it was
 * made). */
export function activityText(
  e: Pick<Experiment, 'run_count' | 'last_run_at' | 'created_at'>,
  mode: ViewMode,
  now?: number,
): string {
  const when =
    e.last_run_at != null
      ? `last started ${fmtRelative(e.last_run_at, { now, words: true })}`
      : `created ${fmtRelative(e.created_at, { now, words: true })}`;
  return `${runCountText(e.run_count, mode)} · ${when}`;
}

/**
 * The experiment's one total cost: the sum of its analyses' estimates, marked an estimate while it
 * can still grow (`growing`: an analysis is still running or has jobs not priced yet). Null when no
 * analysis has a cost yet.
 */
export function totalCostText(total: number | null | undefined, growing: boolean): string | null {
  if (total == null) return null;
  return growing ? `${fmtUsd(total)} · estimate` : fmtUsd(total);
}

/** Whether any of `runs` has not ended (its cost and states are still changing). */
export const anyLive = (runs: readonly Pick<RunSummary, 'terminal'>[]): boolean =>
  runs.some((r) => r.terminal === false);

/** One column of the samples table: an analysis, oldest on the left. */
export interface SampleColumn {
  runId: string;
  label: string;
}

/** One sample's row: its state in each column's analysis, null where that analysis lacks it. */
export interface SampleRow {
  id: string;
  cells: (string | null)[];
}

/**
 * The *Samples* table: a row per sample (the server's first-seen order), a column per analysis —
 * oldest first, so a re-run reads left to right — each cell the sample's state in that analysis.
 * Analyses that name no samples get no column.
 */
export function sampleTable(detail: Pick<ExperimentDetail, 'runs' | 'samples'>): {
  columns: SampleColumn[];
  rows: SampleRow[];
} {
  const named = new Set(detail.samples.flatMap((s) => Object.keys(s.by_run)));
  const columns = distinctLabels(
    [...detail.runs]
      .reverse()
      .filter((r) => named.has(r.run_id))
      .map((r) => ({ runId: r.run_id, label: runName(r) })),
  );
  const rows = detail.samples.map((s) => ({
    id: s.id,
    cells: columns.map((c) => s.by_run[c.runId] ?? null),
  }));
  return { columns, rows };
}

/** Columns whose labels two share (two analyses named alike, or unnamed and started the same
 * minute) told apart by the end of their run id, so every header is unique. */
export function distinctLabels(columns: SampleColumn[]): SampleColumn[] {
  const count = new Map<string, number>();
  for (const c of columns) count.set(c.label, (count.get(c.label) ?? 0) + 1);
  return columns.map((c) =>
    (count.get(c.label) ?? 0) > 1 ? { ...c, label: `${c.label} (…${c.runId.slice(-6)})` } : c,
  );
}

/** "Showing the 200 newest of 250 analyses": the detail holds the newest runs only. */
export function cappedRunsText(shown: number, total: number, mode: ViewMode): string {
  return `Showing the ${shown} newest of ${runCountText(total, mode)}.`;
}

/** "24 samples across 3 analyses". */
export function samplesSummary(samples: number, runs: number, mode: ViewMode): string {
  return `${plural(samples, 'sample')} across ${runCountText(runs, mode)}`;
}

/** How many of `runs` need attention (failed). */
export const failedCount = (runs: readonly Pick<RunSummary, 'state'>[]): number =>
  runs.filter((r) => r.state === 'failed').length;

/** How many analyses' results *Results across analyses* loads at first (more on asking). */
export const RESULTS_PAGE = 4;

/**
 * Whether a run can have results yet: it produced outputs, it resumed a run (whose results it
 * reuses, though it may have registered none of its own yet), or it has ended.
 */
export const mayHaveResults = (
  r: Pick<RunSummary, 'outputs_total' | 'terminal' | 'resumed'>,
): boolean => (r.outputs_total ?? 0) > 0 || r.resumed === true || r.terminal === true;

/** The page of experiment `id`. */
export const experimentHref = (id: string): string => `/experiments/${encodeURIComponent(id)}`;

/**
 * `href` (a path, as the app links) with `experiment=<id>` set in its query, keeping its other
 * parameters and any `#fragment`; `href` unchanged when `id` is null.
 */
export function withExperiment(href: string, id: string | null | undefined): string {
  if (!id) return href;
  const url = new URL(href, 'http://app.invalid');
  url.searchParams.set('experiment', id);
  return `${url.pathname}${url.search}${url.hash}`;
}

/**
 * The file name the samples CSV is saved as: `<experiment name>-samples.csv`, the name trimmed and
 * each character a file name cannot hold on some system (`/ \ : * ? " < > |` and control
 * characters) as `-`; `experiment` when nothing is left. The server names the file in its
 * Content-Disposition by the same rule (`samples_csv_filename` in
 * crates/biffix-backend/src/runs/samples_csv.rs; the browser hides that header from a cross-origin
 * fetch), and both test the same cases, so the button and a direct download agree.
 */
export function samplesCsvFilename(name: string): string {
  const safe = name.trim().replace(/[/\\:*?"<>|\p{Cc}]/gu, '-');
  return `${safe || 'experiment'}-samples.csv`;
}

/**
 * Why the samples CSV cannot be downloaded, or null when it can. The detail's samples cover its
 * newest runs only, so when it was cut short an older run may still have samples: allowed then.
 */
export function samplesCsvUnavailable(
  detail: Pick<ExperimentDetail, 'runs' | 'samples' | 'runs_total'>,
  mode: ViewMode,
): string | null {
  if (detail.samples.length > 0 || detail.runs_total > detail.runs.length) return null;
  const runs = t('runsLower', mode);
  return detail.runs_total === 0
    ? `No samples yet: there are no ${runs} in it.`
    : `No samples yet: none of its ${runs} has named a sample.`;
}

/** What the samples CSV holds, for the download button's description. */
export function samplesCsvAbout(mode: ViewMode): string {
  const run = t('runLower', mode);
  return `One row per sample per ${run}, for every ${run} in it; ${t('runsLower', mode)} that name no samples yet are left out.`;
}
