// What to call an analysis and its pipeline, wherever they show (My work, Analyses, the bell, an
// experiment, the analysis page and its tab title). A run without a name of its own (one started
// from the CLI or a watcher) gets a readable one derived from its pipeline, the
// time it started and its samples — never the raw run id, which no one can tell apart. Pure, so it
// is unit-tested.

import { plural } from '$lib/util/format';

/** The fields a name is read from: a run summary, its detail, or a notification's. */
export interface NameSource {
  run_id: string;
  run_name?: string | null;
  workflow_name?: string | null;
  /** The pipeline's friendly title (its `biffix.yaml` `title`), when it has one. */
  workflow_title?: string | null;
  /** Epoch seconds. */
  submitted_at?: number | null;
  /** How many samples it has (a summary's `samples.total`, a notification's `samples_total`). */
  sampleCount?: number | null;
}

/** The pipeline as a person names it: its friendly title, else its registry name; `null` if none. */
export function pipelineTitle(
  p: { workflow_title?: string | null; workflow_name?: string | null } | null | undefined,
): string | null {
  return p?.workflow_title?.trim() || p?.workflow_name?.trim() || null;
}

/** "Oct 1" (with the year when it is not `now`'s): the day an analysis started. */
export function shortDay(epochSecs: number, now: Date = new Date()): string {
  const d = new Date(epochSecs * 1000);
  const opts: Intl.DateTimeFormatOptions = { month: 'short', day: 'numeric' };
  if (d.getFullYear() !== now.getFullYear()) opts.year = 'numeric';
  return d.toLocaleDateString('en-US', opts);
}

/** "Oct 1 14:05" (24-hour, local time; with the year when not `now`'s): when an analysis started,
 * to the minute, so two started the same day read apart. */
export function startTime(epochSecs: number, now: Date = new Date()): string {
  const d = new Date(epochSecs * 1000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${shortDay(epochSecs, now)} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * The name derived for a run without one: `<pipeline> · <start> · <N samples>` (`Bulk RNA-seq ·
 * Oct 1 14:05 · 3 samples`), leaving out what is not known; `null` when neither its pipeline nor
 * its start is known. The server names unnamed analyses the same way in an experiment's samples
 * CSV (`crates/biffix-backend/src/runs/samples_csv.rs` `analysis_name`), at the UTC offset the UI
 * sends.
 */
export function derivedName(s: NameSource, now: Date = new Date()): string | null {
  const pipeline = pipelineTitle(s);
  const start = s.submitted_at != null ? startTime(s.submitted_at, now) : null;
  if (!pipeline && !start) return null;
  const n = s.sampleCount ?? 0;
  return [pipeline, start, n > 0 ? plural(n, 'sample') : null].filter(Boolean).join(' · ');
}

/** The analysis's name: its own, else the derived one, else (nothing known) its id. */
export function analysisName(s: NameSource, now: Date = new Date()): string {
  return s.run_name?.trim() || derivedName(s, now) || s.run_id;
}

/** Whether `s` shows a derived name (it has no name of its own). */
export function isDerivedName(s: NameSource): boolean {
  return !s.run_name?.trim();
}

/** A run summary (or detail) as a [`NameSource`]. */
export function runNameSource(r: {
  run_id: string;
  run_name?: string | null;
  workflow_name?: string | null;
  workflow_title?: string | null;
  submitted_at?: number | null;
  samples?: { total: number } | null;
}): NameSource {
  return {
    run_id: r.run_id,
    run_name: r.run_name,
    workflow_name: r.workflow_name,
    workflow_title: r.workflow_title,
    submitted_at: r.submitted_at,
    sampleCount: r.samples?.total ?? null,
  };
}

/** A run summary's (or detail's) name. */
export function runName(r: Parameters<typeof runNameSource>[0], now: Date = new Date()): string {
  return analysisName(runNameSource(r), now);
}
