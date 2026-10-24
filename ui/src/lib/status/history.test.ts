import { describe, expect, it } from 'vitest';

import type { StatusHistory } from '#lib/api/index.js';
import {
  MAX_BUCKETS,
  barTiers,
  bucketTimeline,
  historyByComponent,
  uptime,
  type Bucket,
} from './history';

const hist = (over: Partial<StatusHistory> = {}): StatusHistory => ({
  bucket_secs: 3600,
  window_secs: 4 * 3600,
  expected_per_bucket: 12,
  grace_secs: 300,
  components: [],
  ...over,
});

describe('bucketTimeline', () => {
  it('is empty without a history', () => {
    expect(bucketTimeline(null, 0)).toEqual([]);
  });

  it('is empty for a zero, negative or NaN bucket width (no unbounded loop)', () => {
    expect(bucketTimeline(hist({ bucket_secs: 0 }), 1e12)).toEqual([]);
    expect(bucketTimeline(hist({ bucket_secs: -60 }), 1e12)).toEqual([]);
    expect(bucketTimeline(hist({ bucket_secs: NaN }), 1e12)).toEqual([]);
  });

  it('ends at the bucket holding now, aligned to the epoch, oldest first', () => {
    const now = (10 * 3600 + 1234) * 1000; // 10:20:34 after the epoch
    expect(bucketTimeline(hist(), now)).toEqual([7, 8, 9, 10].map((h) => h * 3600));
  });

  it('caps the bar count for a tiny bucket width', () => {
    expect(bucketTimeline(hist({ bucket_secs: 1e-9 }), 1e12)).toHaveLength(MAX_BUCKETS);
  });

  it('draws at least one bucket', () => {
    expect(bucketTimeline(hist({ window_secs: 10 }), 3600e3)).toEqual([3600]);
  });
});

// The 1h window of 5-minute bars: twelve bars, T0+300 .. T0+3600, the last one current.
const T0 = 1000 * 3600;
const IDLE = 3600;
const RUN = 300;
const NOW_MS = (T0 + 3600 + 100) * 1000;
const hourWindow = { bucket_secs: 300, window_secs: 3600, expected_per_bucket: 1 };
const timeline = bucketTimeline(hist(hourWindow), NOW_MS);
const bar = (offset: number) => timeline.indexOf(T0 + offset);

/** A bucket of `ok` samples at `ats` (epoch seconds), each promising the next within `promise`. */
function sampled(ats: number[], promise: number, lapsed = false, over: Partial<Bucket> = {}) {
  const t = Math.floor(ats[0] / 300) * 300;
  return {
    t,
    ok: ats.length,
    degraded: 0,
    down: 0,
    covered_until: ats[ats.length - 1] + promise,
    lapsed,
    last: 'ok',
    ...over,
  } satisfies Bucket;
}

/** The bars' tiers for one component's buckets (and its sample from before the window). */
function tiers(
  buckets: Bucket[],
  live: string | null = 'ok',
  carried_in?: { covered_until: number; last: 'ok' | 'degraded' | 'down' },
) {
  const h = hist({ ...hourWindow, components: [{ name: 'reconciler', buckets, carried_in }] });
  return barTiers(h, 'reconciler', timeline, live);
}

describe('barTiers', () => {
  it('draws the window as twelve bars, the last the current one', () => {
    expect(timeline).toHaveLength(12);
    expect(timeline[0]).toBe(T0 + 300);
    expect(timeline[11]).toBe(T0 + 3600);
  });

  it('is no data without a history or a component', () => {
    expect(barTiers(null, 'reconciler', timeline, 'ok')).toEqual(Array(12).fill('none'));
    // Only the current bar, from the live status.
    const missing = barTiers(hist(hourWindow), 'reconciler', timeline, 'ok');
    expect(missing).toEqual([...Array(11).fill('none'), 'ok']);
  });

  it('covers every bar of an idle tenant sampled hourly', () => {
    // The hour's sample before the first bar (in the window, not drawn) and the current hour's.
    const idle = [sampled([T0 + 30], IDLE), sampled([T0 + 3630], IDLE)];
    expect(tiers(idle)).toEqual(Array(12).fill('ok'));
    // So too when the sample before the window is the one that covers the start.
    const carried = { covered_until: T0 + 30 + IDLE, last: 'ok' as const };
    expect(tiers([sampled([T0 + 3630], IDLE)], 'ok', carried)).toEqual(Array(12).fill('ok'));
  });

  it('draws a gap longer than the promise grey, then no data', () => {
    const buckets = [
      sampled([T0 + 330], RUN),
      // Twenty minutes late.
      sampled([T0 + 2130], RUN, true),
    ];
    const got = tiers(buckets, 'down');
    // Covered for the promise plus grace: through T0+930, so the T0+900 bar only partly.
    expect(got.slice(bar(300), bar(900))).toEqual(['ok', 'ok']);
    expect(got[bar(900)]).toBe('partial');
    expect(got.slice(bar(1200), bar(2100))).toEqual(['none', 'none', 'none']);
    // The late sample's bar missed its start.
    expect(got[bar(2100)]).toBe('partial');
    expect(got[bar(2400)]).toBe('ok');
    expect(got[bar(2700)]).toBe('partial');
    expect(got.slice(bar(3000), bar(3600))).toEqual(['none', 'none']);
    // The current bar, with no sample, takes the live status.
    expect(got[bar(3600)]).toBe('down');
  });

  it('holds a run’s 5-minute promise, then the idle hour’s, within mixed samples', () => {
    const run = [T0 + 1230, T0 + 1530, T0 + 1830];
    const mixed = [
      sampled([T0 + 30], IDLE),
      ...run.map((at) => sampled([at], RUN)),
      // The run ended: the next tick is idle, and promises the hour.
      sampled([T0 + 2130], IDLE),
    ];
    expect(tiers(mixed)).toEqual(Array(12).fill('ok'));
    // A run's sample promises the next tick, so the hour's earlier promise does not cover its
    // missed ticks.
    const stopped = [sampled([T0 + 30], IDLE), sampled([T0 + 1230], RUN)];
    const got = tiers(stopped);
    expect(got.slice(bar(300), bar(1800))).toEqual(Array(5).fill('ok'));
    expect(got[bar(1800)]).toBe('partial');
    expect(got.slice(bar(2100), bar(3600))).toEqual(Array(5).fill('none'));
  });

  it('draws the bars a sample covers at its tier, an unknown one not as healthy', () => {
    const down = [sampled([T0 + 330], IDLE, false, { ok: 0, down: 1, last: 'down' })];
    expect(tiers(down, 'ok').slice(0, 11)).toEqual(Array(11).fill('down'));
    const odd = { covered_until: T0 + 30 + IDLE, last: 'maintenance' as 'ok' };
    expect(tiers([], 'ok', odd).slice(0, 11)).toEqual(Array(11).fill('degraded'));
  });

  it('shows an incident even in a bucket that missed samples', () => {
    const late = sampled([T0 + 330], RUN, true, { ok: 0, down: 1 });
    expect(tiers([late])[bar(300)]).toBe('down');
    const degraded = sampled([T0 + 330], RUN, true, { ok: 0, degraded: 1 });
    expect(tiers([degraded])[bar(300)]).toBe('degraded');
  });

  it('fills an empty current bar from the live status, and keeps a sampled one', () => {
    const before = [sampled([T0 + 3330], RUN)];
    expect(tiers(before, 'down')[11]).toBe('down');
    expect(tiers(before, 'degraded')[11]).toBe('degraded');
    // The probe failed: the last good status is not now's.
    expect(tiers(before, null)[11]).toBe('none');
    // A status this UI does not know is not claimed healthy.
    expect(tiers(before, 'maintenance')[11]).toBe('degraded');
    // A sampled current bar is not yet due its next sample, but a late one there is a gap.
    expect(tiers([sampled([T0 + 3630], RUN)], 'down')[11]).toBe('ok');
    expect(tiers([sampled([T0 + 3630], RUN, true)], 'ok')[11]).toBe('partial');
  });
});

describe('historyByComponent and uptime', () => {
  const at = (t: number, ok: number, degraded: number, down: number): Bucket => ({
    t,
    ok,
    degraded,
    down,
    covered_until: t + 3600,
    lapsed: false,
    last: 'ok',
  });
  const h = hist({
    components: [
      { name: 'api', buckets: [at(0, 9, 1, 0), at(3600, 10, 0, 0)] },
      { name: 'database', buckets: [at(0, 0, 0, 0)] },
    ],
  });
  const map = historyByComponent(h);

  it('keys each component by bucket start', () => {
    expect(map.get('api')?.get(3600)).toEqual(at(3600, 10, 0, 0));
  });

  it('counts degraded samples against uptime', () => {
    expect(uptime(map.get('api'))).toBeCloseTo(95);
  });

  it('is null with no samples or no history', () => {
    expect(uptime(map.get('database'))).toBeNull();
    expect(uptime(map.get('missing'))).toBeNull();
  });
});
