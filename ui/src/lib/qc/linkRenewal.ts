// Keeping the genome browser's links working. igv reads the files through
// presigned links that stop working at `expires_at`, or sooner: a link dies with the Lambda
// session that signed it. So the viewer asks for new links shortly before they expire, and again
// when igv's own reads are refused (403). Free of igv and of the page, so the rules are unit-tested
// with fake timers and a fake XMLHttpRequest.

import type { AlignmentLinks } from '$lib/api';
import { errorMessage } from '$lib/util/format';

/** Re-mint this long before the links expire. */
export const REMINT_LEAD_SECONDS = 120;

/**
 * How long until the links should be asked for again: `REMINT_LEAD_SECONDS` before `expiresAt`
 * (epoch seconds), never negative, and never sooner than `minMs` (so links that are already old
 * are re-minted soon, not in a tight loop).
 */
export function remintDelayMs(expiresAt: number, nowMs: number, minMs = 5_000): number {
  return Math.max(minMs, (expiresAt - REMINT_LEAD_SECONDS) * 1000 - nowMs);
}

/** After a failed renewal, try again this long after, while the links have not yet expired. */
export const RENEW_RETRY_MS = 30_000;

/**
 * Renew for refused reads at most this often: links refused again straight after a renewal are
 * refused for another reason than their age, and asking again would loop.
 */
export const REFUSED_RENEW_INTERVAL_MS = 60_000;

/**
 * Renew on schedule at most this often. `expires_at` is in the server's clock: on a client whose
 * clock runs ahead, new links already look about to expire, and the schedule alone would renew
 * (and reload igv) every few seconds. Links that really die sooner are renewed when a read is
 * refused.
 */
export const MIN_RENEW_INTERVAL_MS = 5 * 60_000;

/**
 * What the viewer says about its links, without closing the genome browser: nothing; a renewal
 * that failed (and whether it will try again by itself); or reads refused even by new links.
 */
export type RenewalNotice =
  null | { kind: 'failed'; message: string; retrying: boolean } | { kind: 'refused' };

/** What the viewer says for `n`, as one or two sentences (nothing for `null`). */
export function noticeText(n: RenewalNotice): string {
  if (n === null) return '';
  if (n.kind === 'refused')
    return 'The storage refused the links to this file again, so some reads may not load.';
  const why = n.message.trim().replace(/[.\s]+$/, '');
  return `Couldn't renew the links to this file: ${why}. ${
    n.retrying
      ? 'Trying again shortly; what is shown keeps working until the links expire.'
      : 'Reads may stop loading once the links expire: try again, or close the viewer and open it again.'
  }`;
}

export interface RenewalDeps {
  /** New links (without the locus: igv keeps its place). */
  fetch: () => Promise<AlignmentLinks>;
  /** Shows `fresh` in igv: swaps the view, and reloads igv's session where it is. */
  apply: (fresh: AlignmentLinks) => Promise<void>;
  /** Says how the links stand (`null`: nothing to say). */
  notice: (n: RenewalNotice) => void;
  now?: () => number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (timer: unknown) => void;
}

/**
 * The links of one open genome browser, renewed `REMINT_LEAD_SECONDS` before they expire (but not
 * within `MIN_RENEW_INTERVAL_MS` of the last links), when a tab that slept past that is shown again
 * (`shown`), when a read is refused (`refused`, at most once per `REFUSED_RENEW_INTERVAL_MS`), or
 * on request (`renew`). A failed renewal leaves igv as it is and is retried every `RENEW_RETRY_MS`
 * while the old links last. `stop` when the viewer closes.
 */
export class LinkRenewal {
  #expiresAt: number;
  /** When (this client's clock) the current links arrived. */
  #receivedAt: number;
  #timer: unknown = null;
  #inFlight = false;
  #stopped = false;
  #lastRefusedRenewal = -Infinity;
  /** Whether igv is loading the new links (a refused read now is theirs, not the old links'). */
  #applying = false;
  /** Whether a read of the new links was refused while igv loaded them. */
  #refusedWhileApplying = false;
  /** What the viewer was last told. */
  #said: RenewalNotice = null;
  /** Renewals failed in a row since the last links arrived. */
  #failures = 0;
  readonly #deps: Required<RenewalDeps>;

  constructor(expiresAt: number, deps: RenewalDeps) {
    this.#deps = {
      now: () => Date.now(),
      setTimer: (fn, ms) => setTimeout(fn, ms),
      clearTimer: (t) => clearTimeout(t as ReturnType<typeof setTimeout>),
      ...deps,
    };
    this.#expiresAt = expiresAt;
    this.#receivedAt = this.#deps.now();
    this.#scheduleRenewal();
  }

  /** New links now. One renewal at a time; a call while one is under way is that one. */
  async renew(): Promise<void> {
    if (this.#inFlight || this.#stopped) return;
    this.#inFlight = true;
    this.#refusedWhileApplying = false;
    this.#clear();
    let renewed = false;
    try {
      const fresh = await this.#deps.fetch();
      if (this.#stopped) return;
      this.#applying = true;
      await this.#deps.apply(fresh);
      if (this.#stopped) return;
      this.#expiresAt = fresh.expires_at;
      this.#receivedAt = this.#deps.now();
      this.#failures = 0;
      this.#say(null);
      this.#scheduleRenewal();
      renewed = true;
    } catch (e) {
      if (this.#stopped) return;
      // Tried again while the links last. Links that looked expired when they arrived say this
      // clock is ahead of the server's, so it cannot tell when they expire: tried again once.
      this.#failures += 1;
      const retrying = this.#clockAhead()
        ? this.#failures === 1
        : this.#expiresAt * 1000 > this.#deps.now() + RENEW_RETRY_MS;
      this.#say({ kind: 'failed', message: errorMessage(e), retrying });
      if (retrying) this.#schedule(RENEW_RETRY_MS);
    } finally {
      this.#inFlight = false;
      this.#applying = false;
    }
    // The new links were refused as igv loaded them: as a refused read now.
    if (renewed && this.#refusedWhileApplying) this.refused();
  }

  /**
   * One of igv's reads was refused (403): renew, unless that was just done for the same reason. A
   * read refused while new links are being fetched was of the old ones, which those replace; one
   * refused while igv loads the new links counts once they are in.
   */
  refused(): void {
    if (this.#stopped) return;
    if (this.#inFlight) {
      if (this.#applying) this.#refusedWhileApplying = true;
      return;
    }
    const now = this.#deps.now();
    if (now - this.#lastRefusedRenewal < REFUSED_RENEW_INTERVAL_MS) {
      // Said once: igv keeps reading, and a notice that changes under the person is unusable.
      if (this.#said?.kind !== 'refused') this.#say({ kind: 'refused' });
      return;
    }
    this.#lastRefusedRenewal = now;
    void this.renew();
  }

  /** The tab is shown again: renew if that was due while it slept (background timers lag). */
  shown(): void {
    const now = this.#deps.now();
    if (
      remintDelayMs(this.#expiresAt, now, 0) === 0 &&
      now - this.#receivedAt >= MIN_RENEW_INTERVAL_MS
    )
      void this.renew();
  }

  stop(): void {
    this.#stopped = true;
    this.#clear();
  }

  /** Whether the current links looked expired when they arrived (this clock runs ahead). */
  #clockAhead(): boolean {
    return this.#expiresAt * 1000 <= this.#receivedAt;
  }

  #say(n: RenewalNotice) {
    this.#said = n;
    this.#deps.notice(n);
  }

  /** The next renewal on schedule, for the current links. */
  #scheduleRenewal() {
    const sinceReceived = this.#deps.now() - this.#receivedAt;
    this.#schedule(
      Math.max(
        remintDelayMs(this.#expiresAt, this.#deps.now()),
        MIN_RENEW_INTERVAL_MS - sinceReceived,
      ),
    );
  }

  #schedule(ms: number) {
    this.#clear();
    if (!this.#stopped) this.#timer = this.#deps.setTimer(() => void this.renew(), ms);
  }

  #clear() {
    if (this.#timer !== null) this.#deps.clearTimer(this.#timer);
    this.#timer = null;
  }
}

/** The part of `XMLHttpRequest` the watch wraps (a fake in tests). */
interface XhrClass {
  prototype: Pick<XMLHttpRequest, 'open'>;
}

/**
 * Calls `onRefused` when a read of a url `isWatched` accepts is refused (403). igv.js reads every
 * file with `XMLHttpRequest` and has no load-error event to hook, so while the viewer is open this
 * wraps `XMLHttpRequest.prototype.open` to listen to those requests (and only those); the function
 * it returns ends the watch, restoring `open` when nothing has wrapped it since.
 */
export function watchRefusedReads(
  isWatched: (url: string) => boolean,
  onRefused: () => void,
  xhr: XhrClass = globalThis.XMLHttpRequest,
): () => void {
  const proto = xhr.prototype;
  const open = proto.open as (this: XMLHttpRequest, ...args: unknown[]) => void;
  let active = true;
  function watched(this: XMLHttpRequest, ...args: unknown[]) {
    const url = args[1];
    if (active && (typeof url === 'string' || url instanceof URL) && isWatched(String(url))) {
      this.addEventListener('loadend', () => {
        if (active && this.status === 403) onRefused();
      });
    }
    return open.apply(this, args);
  }
  proto.open = watched as XMLHttpRequest['open'];
  return () => {
    active = false;
    if (proto.open === (watched as XMLHttpRequest['open'])) proto.open = open;
  };
}
