// The status page's uptime bars: a fixed bucket timeline for the chosen window, each bucket's tier,
// and the uptime figure. Pure functions of the `GET /status/history` payload (and `now`), so the
// rules below are unit-tested rather than living untested in the route.

import type { StatusHistory } from '#lib/api/index.js';
import { healthOfStatus } from './health';

export type Bucket = StatusHistory['components'][number]['buckets'][number];
export type Tier = 'ok' | 'degraded' | 'down' | 'partial' | 'none';

/** The most bars a timeline draws, whatever the payload says. */
export const MAX_BUCKETS = 5000;

/**
 * The fixed bucket timeline for the window (epoch-second bucket starts, oldest→newest), aligned to
 * the bucket interval from the epoch origin so it matches the backend's `date_bin`.
 */
export function bucketTimeline(history: StatusHistory | null, nowMs: number): number[] {
  if (!history) return [];
  const bsec = history.bucket_secs;
  // Guard a degenerate payload: bucket_secs is typed `number`, so a 0 (or negative, or NaN) from the
  // control plane would make `window_secs / bsec` Infinity and hang the tab in an unbounded loop.
  if (!(bsec > 0)) return [];
  // Capped too, so a tiny bucket width can't ask for millions of bars.
  const count = Math.min(MAX_BUCKETS, Math.max(1, Math.round(history.window_secs / bsec)));
  if (!Number.isFinite(count)) return [];
  const latest = Math.floor(nowMs / 1000 / bsec) * bsec;
  const out: number[] = [];
  for (let i = count - 1; i >= 0; i--) out.push(latest - i * bsec);
  return out;
}

/** Each component's buckets, keyed by bucket start. */
export function historyByComponent(
  history: StatusHistory | null,
): Map<string, Map<number, Bucket>> {
  const map = new Map<string, Map<number, Bucket>>();
  for (const c of history?.components ?? []) {
    const byT = new Map<number, Bucket>();
    for (const b of c.buckets) byT.set(b.t, b);
    map.set(c.name, byT);
  }
  return map;
}

/** The tier a status draws, by the health rule the badge reads it by. */
const TIER_OF = { ok: 'ok', warn: 'degraded', bad: 'down' } as const;
const tierOf = (status: string): Tier => TIER_OF[healthOfStatus(status)];

/**
 * A sampled bucket's tier. A real incident shows through regardless of coverage. Otherwise a
 * bucket the probe did not cover throughout is a monitoring data gap, not a clean run, drawn grey
 * ('partial') rather than a false green: a sample in it came later than the one before it promised
 * (`lapsed`), or its newest sample's promise ran out before the bucket ended. The in-progress
 * current bucket has not ended, so only a lapse counts against it.
 */
function sampledTier(b: Bucket, end: number, graceSecs: number, isCurrent: boolean): Tier {
  if (b.down > 0) return 'down';
  if (b.degraded > 0) return 'degraded';
  if (b.lapsed || (!isCurrent && b.covered_until + graceSecs < end)) return 'partial';
  if (b.ok > 0) return 'ok';
  return 'none';
}

/**
 * The tier of each bar of `timeline` (from {@link bucketTimeline}) for one component.
 *
 * The probe samples every 5 minutes while a workflow runs but only hourly on an idle tenant, and
 * each sample promises the next within its `next_within_secs`: a sample covers the time from it
 * until that promise, plus the grace, has passed (what `GET /status` calls fresh). So a bucket with
 * no sample of its own still draws, at the tier of the sample that covers it, while the newest
 * sample before it (or the one from before the window, `carried_in`) covers it to its end; one
 * covered only partway is a gap ('partial'), and one not covered at all is no data ('none').
 *
 * The rightmost bar is the current, in-progress bucket. Without a sample of its own it takes the
 * LIVE component status rather than grey. `liveStatus` is `null` when the latest probe failed (the
 * status shown is the last good one, not now's): then it stays grey. A status this UI does not
 * know is not claimed healthy.
 */
export function barTiers(
  history: StatusHistory | null,
  name: string,
  timeline: number[],
  liveStatus: string | null,
): Tier[] {
  if (!history || timeline.length === 0) return timeline.map(() => 'none');
  const component = history.components.find((c) => c.name === name);
  const grace = history.grace_secs;
  const byT = new Map<number, Bucket>();
  // The coverage so far: how long the newest sample before the bar vouches for (grace included),
  // at what tier.
  let carried = component?.carried_in
    ? { until: component.carried_in.covered_until + grace, last: component.carried_in.last }
    : null;
  for (const b of [...(component?.buckets ?? [])].sort((x, y) => x.t - y.t)) {
    // A bucket in the window but before the first bar is not drawn; it still covers the bars.
    if (b.t < timeline[0]) carried = { until: b.covered_until + grace, last: b.last };
    else byT.set(b.t, b);
  }
  return timeline.map((t, i) => {
    const isCurrent = i === timeline.length - 1;
    const end = t + history.bucket_secs;
    const b = byT.get(t);
    if (b) {
      carried = { until: b.covered_until + grace, last: b.last };
      return sampledTier(b, end, grace, isCurrent);
    }
    if (isCurrent) return liveStatus === null ? 'none' : tierOf(liveStatus);
    if (carried && carried.until >= end) return tierOf(carried.last);
    if (carried && carried.until > t) return 'partial';
    return 'none';
  });
}

/**
 * Uptime in percent: the fraction of samples that were FULLY operational (ok / present). Degraded
 * time counts against it, so a degraded component reads below 100%. `null` with no samples.
 */
export function uptime(byT: Map<number, Bucket> | undefined): number | null {
  if (!byT) return null;
  let present = 0;
  let ok = 0;
  for (const b of byT.values()) {
    present += b.ok + b.degraded + b.down;
    ok += b.ok;
  }
  if (present === 0) return null;
  return (ok / present) * 100;
}
