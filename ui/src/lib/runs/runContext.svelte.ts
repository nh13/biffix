// The run a `/runs/[id]` page shows, shared from the route's layout (which polls it) to every view
// beneath it, so the header and each view read one copy instead of fetching their own.

import { getContext, setContext } from 'svelte';
import { cancelRun, type RunDetail, type RunReport } from '#lib/api/index.js';
import type { ErrorKind } from '#lib/components/errorPage.js';
import { errorMessage } from '#lib/util/format.js';

/**
 * What the run's action buttons are doing. The header and the pinned bar each show the buttons;
 * sharing this makes them one set, so a Cancel in flight disables both (it cannot be sent twice)
 * and a failure is reported once.
 */
export class RunActionsState {
  cancelling = $state(false);
  savingProvenance = $state(false);
  /** The last action's failure, until the next action starts. */
  error = $state<string | null>(null);

  /**
   * Cancel `runId` once the user confirms `question`: terminates its coordinator, and the poll then
   * shows the run ending as cancelled. One cancel at a time; a failure is kept in `error`.
   */
  async cancel(runId: string, question: string): Promise<void> {
    if (this.cancelling) return;
    if (!confirm(question)) return;
    this.cancelling = true;
    this.error = null;
    try {
      await cancelRun(runId);
    } catch (e) {
      this.error = errorMessage(e);
      this.cancelling = false;
    }
  }

  /** Back to idle (another run is shown). */
  reset(): void {
    this.cancelling = false;
    this.savingProvenance = false;
    this.error = null;
  }
}

export class RunState {
  /** The latest detail; null until the first load succeeds. */
  run = $state<RunDetail | null>(null);
  /** True until the first load settles. */
  loading = $state(true);
  /** The first load's failure (background refresh failures keep the last good view). */
  error = $state<string | null>(null);
  /** What kind of failure `error` is (not found, no access, …), for the error page's words. */
  errorKind = $state<ErrorKind>('server');
  /** Bumped to load the run again after its first load failed. */
  loadAttempt = $state(0);
  /** Why the last background refresh failed (what is shown may be out of date); null once one
   * succeeds. */
  stale = $state<string | null>(null);
  /** The run's reports; null until loaded (a failed load keeps the last good list, or null). */
  reports = $state<RunReport[] | null>(null);
  /** Why the latest reports load failed; null while loading or once one succeeds. */
  reportsError = $state<string | null>(null);
  /** Bumped to load the reports again (after a failure). */
  reportsAttempt = $state(0);
  /**
   * A resumed run's output count with the files it reused from the runs it resumed (the outputs
   * list's `total`), which its projection's `outputs_total` (what it registered itself) leaves out.
   * Null for a run that resumed nothing (`outputs_total` is its count), and until loaded.
   */
  outputsTotal = $state<number | null>(null);
  /** Ask for the run's reports again. */
  reloadReports(): void {
    this.reportsAttempt += 1;
  }
  /** The run's actions (Cancel, Provenance), one set however many places show the buttons. */
  actions = new RunActionsState();
  /** A rename just saved, and when: a poll that was in flight before it must not undo it. */
  #renamed: { runId: string; name: string | null; at: number } | null = null;

  /** Show the name a rename just saved (`null`: cleared). */
  applyRename(runId: string, name: string | null, now = Date.now()): void {
    this.#renamed = { runId, name, at: now };
    if (this.run && this.run.run_id === runId) this.run = { ...this.run, run_name: name };
  }

  /**
   * A polled detail with a rename just saved kept: a poll answered with the old name (it was in
   * flight before the rename) keeps the new one, until a poll returns it or {@link RENAME_HOLD_MS}
   * pass (a later rename elsewhere then shows).
   */
  withRename(r: RunDetail, now = Date.now()): RunDetail {
    const held = this.#renamed;
    if (!held || held.runId !== r.run_id) return r;
    if ((r.run_name?.trim() || null) === held.name || now - held.at > RENAME_HOLD_MS) {
      this.#renamed = null;
      return r;
    }
    return { ...r, run_name: held.name };
  }
}

/** How long a saved rename outranks a polled name that differs (several poll beats). */
export const RENAME_HOLD_MS = 15_000;

const KEY = Symbol('run');

/** Provide `ctx` to the components beneath the caller. */
export function setRunContext(ctx: RunState): RunState {
  return setContext(KEY, ctx);
}

/** The run provided by the enclosing `/runs/[id]` layout. */
export function getRunContext(): RunState {
  const ctx = getContext<RunState | undefined>(KEY);
  if (!ctx) throw new Error('getRunContext() outside a /runs/[id] layout');
  return ctx;
}
