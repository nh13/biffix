import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AlignmentLinks } from '#lib/api/index.js';
import {
  LinkRenewal,
  MIN_RENEW_INTERVAL_MS,
  REFUSED_RENEW_INTERVAL_MS,
  REMINT_LEAD_SECONDS,
  remintDelayMs,
  RENEW_RETRY_MS,
  noticeText,
  watchRefusedReads,
  type RenewalNotice,
} from './linkRenewal';

const HOUR_S = 3600;
const viewExpiring = (expiresAt: number, url = 'https://s3/S1.bam?sig=new') =>
  ({ alignment: { url }, expires_at: expiresAt }) as AlignmentLinks;

describe('LinkRenewal', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000_000);
  });
  afterEach(() => vi.useRealTimers());

  const nowS = () => Date.now() / 1000;

  /** A renewal whose fetches answer `answers` in turn (an Error rejects), recording what it did. */
  function renewalWith(answers: (AlignmentLinks | Error)[], expiresAt = nowS() + HOUR_S) {
    const applied: AlignmentLinks[] = [];
    const notices: RenewalNotice[] = [];
    const fetch = vi.fn(async () => {
      const next = answers.shift();
      if (!next) throw new Error('no more answers');
      if (next instanceof Error) throw next;
      return next;
    });
    const renewal = new LinkRenewal(expiresAt, {
      fetch,
      apply: async (fresh) => void applied.push(fresh),
      notice: (n) => void notices.push(n),
    });
    return { renewal, fetch, applied, notices };
  }

  it('renews the links shortly before they expire, then before the new ones do', async () => {
    const first = viewExpiring(nowS() + 2 * HOUR_S);
    const { fetch, applied, notices } = renewalWith([first, viewExpiring(nowS() + 4 * HOUR_S)]);
    await vi.advanceTimersByTimeAsync((HOUR_S - REMINT_LEAD_SECONDS) * 1000 - 1);
    expect(fetch).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(applied).toEqual([first]);
    expect(notices).toEqual([null]);
    // The next renewal is scheduled from the new links' expiry (an hour later), not the old one's.
    await vi.advanceTimersByTimeAsync(HOUR_S * 1000 - 1);
    expect(fetch).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('says a failed renewal without applying anything, and tries again while the links last', async () => {
    const fresh = viewExpiring(nowS() + 2 * HOUR_S);
    const { renewal, fetch, applied, notices } = renewalWith([new Error('offline'), fresh]);
    await renewal.renew();
    expect(applied).toEqual([]);
    expect(notices).toEqual([{ kind: 'failed', message: 'offline', retrying: true }]);
    await vi.advanceTimersByTimeAsync(RENEW_RETRY_MS);
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(applied).toEqual([fresh]);
    expect(notices.at(-1)).toBeNull();
  });

  it('stops trying by itself once the links have expired', async () => {
    const { renewal, fetch, notices } = renewalWith([new Error('offline')], nowS() + 10);
    await renewal.renew();
    expect(notices).toEqual([{ kind: 'failed', message: 'offline', retrying: false }]);
    await vi.advanceTimersByTimeAsync(10 * RENEW_RETRY_MS);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it('tries again once after a failure on a clock ahead of the server, where links look expired', async () => {
    const fresh = viewExpiring(nowS() + HOUR_S);
    // This clock is ahead: the links look expired on arrival, though they work.
    const { renewal, fetch, notices } = renewalWith(
      [new Error('offline'), new Error('offline'), fresh],
      nowS() - 60,
    );
    await renewal.renew();
    expect(notices).toEqual([{ kind: 'failed', message: 'offline', retrying: true }]);
    await vi.advanceTimersByTimeAsync(RENEW_RETRY_MS);
    expect(fetch).toHaveBeenCalledTimes(2);
    // A second failure in a row, on links that look expired: it stops trying by itself.
    expect(notices.at(-1)).toEqual({ kind: 'failed', message: 'offline', retrying: false });
    await vi.advanceTimersByTimeAsync(10 * RENEW_RETRY_MS);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('renews once for a burst of refused reads, and says so when new links are refused too', async () => {
    const { renewal, fetch, notices } = renewalWith([
      viewExpiring(nowS() + HOUR_S),
      viewExpiring(nowS() + HOUR_S),
    ]);
    renewal.refused();
    renewal.refused();
    renewal.refused();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(notices).toEqual([null]);
    // Refused again straight after: no loop, the viewer says so.
    renewal.refused();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(notices.at(-1)).toEqual({ kind: 'refused' });
    // Said once, however many reads are refused.
    renewal.refused();
    renewal.refused();
    expect(notices.filter((n) => n?.kind === 'refused')).toHaveLength(1);
    // A minute on, a refused read is worth new links again.
    await vi.advanceTimersByTimeAsync(REFUSED_RENEW_INTERVAL_MS);
    renewal.refused();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('counts a read refused while igv loads the new links, not one of the old links', async () => {
    let renewal: LinkRenewal | null = null;
    let refuseOnApply = false;
    const notices: RenewalNotice[] = [];
    const fetch = vi.fn(async () => {
      // A read of the old links refused while the new ones are fetched: theirs, ignored.
      renewal?.refused();
      return viewExpiring(Date.now() / 1000 + HOUR_S);
    });
    renewal = new LinkRenewal(nowS() + HOUR_S, {
      fetch,
      apply: async () => {
        if (refuseOnApply) renewal?.refused();
      },
      notice: (n) => void notices.push(n),
    });
    await renewal.renew();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(notices).toEqual([null]);
    // The new links refused as igv loads them: renewed again (the first refusal), then said.
    refuseOnApply = true;
    await renewal.renew();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).toHaveBeenCalledTimes(3);
    expect(notices.at(-1)).toEqual({ kind: 'refused' });
  });

  it('renews when a tab that slept past the time is shown again, and not before', async () => {
    // A background tab's timers lag: this one never fires.
    const fetch = vi.fn(async () => viewExpiring(nowS() + HOUR_S));
    const renewal = new LinkRenewal(nowS() + 10 * 60, {
      fetch,
      apply: async () => {},
      notice: () => {},
      setTimer: () => null,
      clearTimer: () => {},
    });
    vi.setSystemTime(Date.now() + 60_000);
    renewal.shown();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).not.toHaveBeenCalled();
    vi.setSystemTime(Date.now() + 8 * 60_000);
    renewal.shown();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it('renews every few minutes, not every few seconds, on a clock ahead of the server', async () => {
    // Links that look expired on arrival (this clock is an hour ahead of the server's).
    const behind = () => viewExpiring(nowS() - 60);
    const { renewal, fetch } = renewalWith([behind(), behind(), behind()], nowS() - 60);
    await vi.advanceTimersByTimeAsync(MIN_RENEW_INTERVAL_MS - 1);
    expect(fetch).not.toHaveBeenCalled();
    // Shown again: the links look due, but the last ones came too recently.
    renewal.shown();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetch).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(fetch).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(MIN_RENEW_INTERVAL_MS);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('does nothing once stopped, even with a renewal under way', async () => {
    let answer: (v: AlignmentLinks) => void = () => {};
    const apply = vi.fn(async () => {});
    const notice = vi.fn();
    const renewal = new LinkRenewal(nowS() + HOUR_S, {
      fetch: () => new Promise((resolve) => (answer = resolve)),
      apply,
      notice,
    });
    const under = renewal.renew();
    renewal.stop();
    answer(viewExpiring(nowS() + HOUR_S));
    await under;
    renewal.refused();
    await vi.advanceTimersByTimeAsync(2 * HOUR_S * 1000);
    expect(apply).not.toHaveBeenCalled();
    expect(notice).not.toHaveBeenCalled();
  });
});

describe('watchRefusedReads', () => {
  /** A stand-in for XMLHttpRequest: `open` records the url; `finish` ends it with a status. */
  class FakeXhr {
    status = 0;
    url = '';
    #listeners: (() => void)[] = [];
    open(_method: string, url: string) {
      this.url = url;
    }
    addEventListener(type: string, fn: () => void) {
      if (type === 'loadend') this.#listeners.push(fn);
    }
    finish(status: number) {
      this.status = status;
      for (const fn of this.#listeners) fn();
    }
  }
  const request = (url: string) => {
    const x = new FakeXhr();
    (x.open as (m: string, u: string) => void)('GET', url);
    return x;
  };
  const ownOpen = FakeXhr.prototype.open;
  const watchFake = (isWatched: (url: string) => boolean, onRefused: () => void) =>
    watchRefusedReads(isWatched, onRefused, FakeXhr as unknown as typeof XMLHttpRequest);

  it("calls back on a refused read of the viewer's own links only, until it is ended", () => {
    const refused = vi.fn();
    const stop = watchFake((url) => url.startsWith('https://s3/'), refused);
    request('https://s3/S1.bam?sig=1').finish(206);
    request('https://igv.org/genomes.json').finish(403);
    expect(refused).not.toHaveBeenCalled();
    const read = request('https://s3/S1.bam?sig=1');
    expect(read.url).toBe('https://s3/S1.bam?sig=1');
    read.finish(403);
    expect(refused).toHaveBeenCalledTimes(1);
    stop();
    expect(FakeXhr.prototype.open).toBe(ownOpen);
    request('https://s3/S1.bam?sig=1').finish(403);
    expect(refused).toHaveBeenCalledTimes(1);
  });

  it('leaves a later wrapper in place when it ends, and stops listening all the same', () => {
    const refused = vi.fn();
    const stop = watchFake(() => true, refused);
    const ours = FakeXhr.prototype.open;
    const later = function (this: FakeXhr, m: string, u: string) {
      return ours.call(this, m, u);
    };
    FakeXhr.prototype.open = later;
    stop();
    expect(FakeXhr.prototype.open).toBe(later);
    request('https://s3/S1.bam').finish(403);
    expect(refused).not.toHaveBeenCalled();
    FakeXhr.prototype.open = ownOpen;
  });
});

describe('noticeText', () => {
  it('says what happens next, in one sentence each, without a doubled period', () => {
    expect(noticeText({ kind: 'failed', message: 'Service Unavailable.', retrying: true })).toBe(
      "Couldn't renew the links to this file: Service Unavailable. Trying again shortly; what is shown keeps working until the links expire.",
    );
    expect(noticeText({ kind: 'failed', message: 'offline', retrying: false })).toBe(
      "Couldn't renew the links to this file: offline. Reads may stop loading once the links expire: try again, or close the viewer and open it again.",
    );
    expect(noticeText({ kind: 'refused' })).toBe(
      'The storage refused the links to this file again, so some reads may not load.',
    );
    expect(noticeText(null)).toBe('');
  });
});

describe('remintDelayMs', () => {
  it('re-mints two minutes before the links expire, never in a tight loop', () => {
    const now = 1_000_000_000;
    expect(remintDelayMs(now / 1000 + 3600, now)).toBe(3480_000);
    expect(remintDelayMs(now / 1000 + 60, now)).toBe(5_000);
    expect(remintDelayMs(now / 1000 + 60, now, 0)).toBe(0);
  });
});
