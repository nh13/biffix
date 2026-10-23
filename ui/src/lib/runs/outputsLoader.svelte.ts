// One run's registered outputs, as the Expert view's Outputs tab and the Lab view's *All files*
// both read them: every page up to a cap, then again while the run is live (new files keep
// arriving). The two views draw them differently; the reading, paging and polling live here, so a
// fix to one (a paging cap, a poll edge case) is a fix to both.
//
// A component owns one loader and starts it from an `$effect`, returning the stop this hands back:
//
//   const files = new OutputsLoader();
//   $effect(() => files.watch({ runId, live }));

import { untrack } from 'svelte';
import { listAllOutputs, listOutputs, type RunOutput } from '$lib/api';
import { startPoll } from '$lib/util/poll';
import { sameOutputs } from '$lib/runs/outputs';

/** Rows per request. */
const PAGE = 2000;
/** Enough for any results set a person browses; a run with more is paged but not all read. */
const MAX_ROWS = 10_000;
/** How often a live run's outputs are read again. */
const REFRESH_MS = 16_000;
// S3 sizes arrive a bounded batch per request; re-ask quickly until they have all come in.
const SIZING_MS = 2_500;
const MAX_SIZING_POLLS = 40;

/** Which of the run's outputs to read. */
export interface OutputsQuery {
  /** Only this task's outputs (filtered by the server, so the cap and `total` are the task's). */
  task?: string;
  /** Roles to include (`*` ⇒ every role); omitted ⇒ everything except work-dir intermediates. */
  roles?: string[];
}

export interface WatchOptions {
  runId: string;
  /** Whether the run is still going: its outputs are read again every `REFRESH_MS` until it ends. */
  live: boolean;
  query?: OutputsQuery;
  /**
   * While the run is live, read the whole list only every `probeEvery` beats; each beat between
   * asks only how many outputs there are (one row), and reads the list again when that changed.
   * Without it (and once the run has ended) every beat reads the whole list.
   */
  probeEvery?: number;
  /** Once the run has ended, keep asking (quickly, a bounded number of times) until every
   * output's size is in. Without it an ended run is read once. */
  waitForSizes?: boolean;
}

interface Read {
  outputs: RunOutput[];
  total: number;
  /** Outputs whose S3 size is still being checked. */
  pending: number;
}

export class OutputsLoader {
  outputs = $state<RunOutput[]>([]);
  /** How many outputs match (may be more than `outputs` holds: see `MAX_ROWS`). */
  total = $state(0);
  /** Outputs whose S3 size is still being checked (the last page's count, the freshest). */
  pending = $state(0);
  /** Whether the `pending` sizes are still being waited for: false once the loader stops asking
   * (an ended run read without `waitForSizes`, or one whose sizes never came), so a view says
   * those sizes are unknown rather than coming. */
  sizing = $state(false);
  loading = $state(true);
  error = $state<string | null>(null);

  /** Forget the last listing (a different run or task is a different listing). */
  clear(): void {
    this.outputs = [];
    this.total = 0;
    this.pending = 0;
    this.sizing = false;
  }

  /** Read the outputs, then keep reading them as `opts` says; returns the stop. */
  watch(opts: WatchOptions): () => void {
    const { runId, live, query = {}, probeEvery, waitForSizes = false } = opts;
    this.loading = true;
    this.error = null;

    const readAll = async (): Promise<Read> => {
      const read = await listAllOutputs(runId, query, { maxRows: MAX_ROWS, pageSize: PAGE });
      return {
        outputs: read.outputs,
        total: read.total,
        pending: read.last?.enrichment_pending ?? 0,
      };
    };
    let beat = 0;
    const read = async (): Promise<Read> => {
      const n = beat++;
      // Only while live: a probe compares the count, and sizes arriving on an ended run never
      // change it.
      if (probeEvery && live && n > 0 && n % probeEvery !== 0) {
        const probe = await listOutputs(runId, { ...query, limit: 1 });
        if (probe.total === this.total)
          return { outputs: this.outputs, total: this.total, pending: this.pending };
      }
      return readAll();
    };

    // An ended run waiting for its sizes is asked at most MAX_SIZING_POLLS times, failed asks
    // included: `until` only sees successes, so a listing that keeps failing is stopped from here.
    let attempts = 0;
    let stop = () => {};
    const fetch = async (): Promise<Read> => {
      attempts += 1;
      try {
        // Untracked: a caller's `$effect` must not depend on the state this reads and writes.
        return await untrack(read);
      } catch (e) {
        if (waitForSizes && !live && attempts >= MAX_SIZING_POLLS) {
          stop();
          this.sizing = false;
        }
        throw e;
      }
    };
    stop = startPoll({
      fetch,
      onData: (r) => {
        // An unchanged listing (the common poll once sizes are in) keeps the rows shown, so what
        // is drawn from them is not rebuilt over thousands of rows for nothing.
        if (!sameOutputs(this.outputs, r.outputs)) this.outputs = r.outputs;
        this.total = r.total;
        this.pending = r.pending;
        this.sizing = r.pending > 0 && (live || (waitForSizes && attempts < MAX_SIZING_POLLS));
        this.error = null;
      },
      onError: (m) => (this.error = m),
      onSettled: () => (this.loading = false),
      // A live run keeps refreshing; an ended one stops, or waits for its sizes.
      until: (r) => !live && (!waitForSizes || r.pending === 0 || attempts >= MAX_SIZING_POLLS),
      intervalMs: live || !waitForSizes ? REFRESH_MS : SIZING_MS,
    });
    return () => stop();
  }
}
