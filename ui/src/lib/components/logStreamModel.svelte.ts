// The state behind the in-app CloudWatch log viewer (LogStream.svelte): the first page (at the tail,
// or at the start of the stream), "load earlier" / "load more" paging, and the live tail. It lives
// here, not in the component, so its sequencing rules are unit-tested:
//
// - Last writer wins. Every first-page load starts a new generation; a response from an older
//   generation (a slow tail poll, a superseded toggle) is dropped rather than appended to a view it
//   no longer belongs to.
// - No overlap. A tail fetch or "load earlier" already in flight is joined, never duplicated, so a
//   page slower than the poll interval can't append the same events twice. A tail fetch for a
//   view a toggle replaced is not joined (its answer is dropped): the new view fetches its own.
// - Until there is a stream, poll the first page. A queued task has no stream yet (and a stream
//   that was just recorded may 404 until CloudWatch creates it); `tick` keeps re-loading the first
//   page, quietly, until one arrives with a forward cursor, then tails from that cursor.
// - CloudWatch paging: an empty page does not mean the end. The end is a returned token equal to
//   the one sent (per GetLogEvents), so "load earlier" follows empty pages a bounded number of times
//   before handing back to the user.
// - Bounded memory. At most `maxLines` lines are kept. At the tail the oldest are dropped (and
//   "load earlier" is withdrawn: its cursor no longer adjoins what is kept); read from the start,
//   the view stops growing instead, so the lines being read never shift away.

import type { LogPage } from '$lib/api';
import { errorMessage } from '$lib/util/format';

export type FetchLogPage = (opts: {
  cursor?: string;
  direction?: 'forward' | 'backward';
  limit?: number;
}) => Promise<LogPage>;

/** One rendered line; `id` is a stable key for the keyed `{#each}`. */
export interface LogLine {
  id: number;
  ts_ms: number;
  message: string;
}

export interface LogStreamOptions {
  /** Events per page. */
  limit?: number;
  /** The most lines kept at once. */
  maxLines?: number;
  /** Empty backward pages followed automatically by one "load earlier". */
  maxEmptyPages?: number;
}

export class LogStreamModel {
  /** Oldest first, always; the view flips the order for "newest first". */
  lines = $state.raw<LogLine[]>([]);
  loading = $state(true);
  loadingEarlier = $state(false);
  loadingMore = $state(false);
  /** A failed first-page load (the view has nothing to show). */
  error = $state<string | null>(null);
  /** A failed "load earlier": shown next to the button, the loaded lines stay. */
  earlierError = $state<string | null>(null);
  /** No stream (yet) — expected for a queued task, not an error. */
  noStream = $state(false);
  /** Reached the oldest event. */
  atStart = $state(false);
  /** Where the view opened: the tail (newest) or the start of the stream. */
  fromStart = $state(false);
  /** Older lines were dropped to stay within `maxLines` (tail view). */
  headTrimmed = $state(false);
  /** The view reached `maxLines` reading from the start and stopped growing. */
  tailCapped = $state(false);
  readonly maxLines: number;

  #fetch: FetchLogPage;
  #limit: number;
  #maxEmptyPages: number;
  #forwardToken: string | undefined;
  #backwardToken: string | undefined;
  #stream: string | null = null;
  #gen = 0;
  #nextId = 0;
  #disposed = false;
  #first: Promise<void> | undefined;
  #tail: Promise<void> | undefined;
  /** The generation `#tail` reads for. */
  #tailGen = -1;
  #earlier: Promise<void> | undefined;

  constructor(fetchPage: FetchLogPage, opts: LogStreamOptions = {}) {
    this.#fetch = fetchPage;
    this.#limit = opts.limit ?? 300;
    this.maxLines = opts.maxLines ?? 5000;
    this.#maxEmptyPages = opts.maxEmptyPages ?? 5;
  }

  /** Whether the live tail can follow from a forward cursor (else `tick` reloads the first page). */
  get tailing(): boolean {
    return !this.noStream && this.error === null && this.#forwardToken !== undefined;
  }

  /**
   * Load the first page for the current `fromStart`, replacing the view. `quiet` (a background
   * retry) neither shows the spinner nor clears a stream already on screen when it fails.
   */
  loadFirst({ quiet = false }: { quiet?: boolean } = {}): Promise<void> {
    const gen = ++this.#gen;
    const start = this.fromStart; // the direction of THIS request, not whatever it is on return
    if (!quiet) {
      this.loading = true;
      this.error = null;
    }
    const run = async () => {
      try {
        const page = await this.#fetch({
          direction: start ? 'forward' : 'backward',
          limit: this.#limit,
        });
        if (gen !== this.#gen) return;
        this.#applyFirst(page, start);
      } catch (e) {
        if (gen !== this.#gen) return;
        // A quiet retry of a view that is already showing "no stream yet" keeps that message: the
        // stream may simply 404 until CloudWatch creates it.
        if (!quiet || this.error !== null) this.error = errorMessage(e);
      } finally {
        if (gen === this.#gen) {
          this.loading = false;
          this.#first = undefined;
        }
      }
    };
    this.#first = run();
    return this.#first;
  }

  /** Flip between the tail and the start of the stream: exactly one first-page load. */
  toggleStart(): Promise<void> {
    this.fromStart = !this.fromStart;
    return this.loadFirst();
  }

  /** One live-tail step: the first page until there is a stream to tail, then newer events. */
  tick(): Promise<void> {
    if (this.#disposed) return Promise.resolve();
    if (this.#first) return this.#first;
    if (!this.tailing) return this.loadFirst({ quiet: true });
    return this.loadNewer();
  }

  /**
   * The stream stopped growing (the task ended): fetch once more after anything in flight, and
   * again after `delayMs` for CloudWatch's ingestion lag. Returns a cancel.
   */
  settle(delayMs: number): () => void {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const inFlight = this.#first ?? this.#tail ?? Promise.resolve();
    void inFlight
      .then(() => (cancelled || this.#disposed ? undefined : this.tick()))
      .then(() => {
        if (!cancelled && !this.#disposed) timer = setTimeout(() => void this.tick(), delayMs);
      });
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }

  /** Append anything newer than what is shown (the live tail, and "load more"). */
  loadNewer(): Promise<void> {
    // Joined only when it reads for this view: one of a view a toggle replaced will be dropped.
    if (this.#tail && this.#tailGen === this.#gen) return this.#tail;
    const cursor = this.#forwardToken;
    if (cursor === undefined || this.tailCapped) return Promise.resolve();
    const gen = this.#gen;
    const run = async () => {
      try {
        const page = await this.#fetch({ direction: 'forward', cursor, limit: this.#limit });
        if (gen !== this.#gen) return;
        // The stream behind this view changed (e.g. a retry moved the display attempt): the
        // cursor belongs to the old one, so start over on the new one.
        if (page.log_stream != null && this.#stream != null && page.log_stream !== this.#stream) {
          void this.loadFirst({ quiet: true });
          return;
        }
        this.#append(page.events ?? []);
        // A forward token that stops advancing just means "caught up" — keep it and poll again.
        if (!this.tailCapped) this.#forwardToken = page.next_forward_token ?? cursor;
      } catch {
        // Transient failure: leave the view as it is and try again on the next tick.
      } finally {
        // Only its own: a superseded tail landing late must not forget the current one.
        if (this.#tailGen === gen) this.#tail = undefined;
      }
    };
    this.#tailGen = gen;
    this.#tail = run();
    return this.#tail;
  }

  /** "Load more" (reading from the start): one forward page, with its own spinner. */
  async loadMore(): Promise<void> {
    this.loadingMore = true;
    try {
      await this.loadNewer();
    } finally {
      this.loadingMore = false;
    }
  }

  /** Prepend the page of events before the oldest shown, following empty pages. */
  loadEarlier(): Promise<void> {
    if (this.#earlier) return this.#earlier;
    const cursor = this.#backwardToken;
    if (cursor === undefined || this.atStart || this.headTrimmed) return Promise.resolve();
    const gen = this.#gen;
    this.loadingEarlier = true;
    this.earlierError = null;
    const run = async () => {
      try {
        let token = cursor;
        let older: LogPage['events'] = [];
        let end = false;
        for (let i = 0; i < this.#maxEmptyPages; i++) {
          const page = await this.#fetch({
            direction: 'backward',
            cursor: token,
            limit: this.#limit,
          });
          if (gen !== this.#gen) return;
          const next = page.next_backward_token ?? undefined;
          // CloudWatch signals the end by handing back the token it was sent; a page can carry
          // events AND be the last one, so the two are decided separately.
          end = next === undefined || next === token;
          older = page.events ?? [];
          if (next !== undefined) token = next;
          if (end || older.length > 0) break;
        }
        this.#backwardToken = token;
        this.atStart = end;
        this.#prepend(older);
      } catch (e) {
        if (gen === this.#gen) this.earlierError = errorMessage(e);
      } finally {
        this.loadingEarlier = false;
        this.#earlier = undefined;
      }
    };
    this.#earlier = run();
    return this.#earlier;
  }

  /** Stop: every response still in flight is dropped, and `tick`/`settle` do nothing more. */
  dispose(): void {
    this.#disposed = true;
    this.#gen++;
  }

  #applyFirst(page: LogPage, start: boolean) {
    this.noStream = page.log_stream == null;
    this.#stream = page.log_stream ?? null;
    this.error = null;
    this.earlierError = null;
    this.headTrimmed = false;
    this.tailCapped = false;
    this.#forwardToken = page.next_forward_token ?? undefined;
    this.#backwardToken = page.next_backward_token ?? undefined;
    // Opened at the start of the stream, there is nothing older to load.
    this.atStart = start;
    this.lines = [];
    this.#append(page.events ?? []);
  }

  #toLines(events: LogPage['events']): LogLine[] {
    return events.map((e) => ({ id: this.#nextId++, ts_ms: e.ts_ms, message: e.message }));
  }

  #append(events: LogPage['events']) {
    if (events.length === 0) return;
    let next = this.lines.concat(this.#toLines(events));
    if (next.length > this.maxLines) {
      if (this.fromStart) {
        // Reading from the start: keep the beginning and stop following.
        next = next.slice(0, this.maxLines);
        this.tailCapped = true;
      } else {
        next = next.slice(next.length - this.maxLines);
        this.#trimHead();
      }
    }
    this.lines = next;
  }

  #prepend(events: LogPage['events']) {
    if (events.length === 0) return;
    let next = this.#toLines(events).concat(this.lines);
    if (next.length > this.maxLines) {
      next = next.slice(next.length - this.maxLines);
      this.#trimHead();
    }
    this.lines = next;
  }

  #trimHead() {
    this.headTrimmed = true;
    this.atStart = false;
    this.#backwardToken = undefined;
  }
}
