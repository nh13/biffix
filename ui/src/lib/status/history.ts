// The status page's uptime bars: a fixed bucket timeline for the chosen window, each bucket's tier,
// and the uptime figure. Pure functions of the `GET /status/history` payload (and `now`), so the
// rules below are unit-tested rather than living untested in the route.

import type { StatusHistory } from '#lib/api/index.js';
import { healthOfStatus } from './health';

export type Bucket = { ok: number; degraded: number; down: number };
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
    for (const b of c.buckets) byT.set(b.t, { ok: b.ok, degraded: b.degraded, down: b.down });
    map.set(c.name, byT);
  }
  return map;
}

/** A bucket's tier from its samples alone. */
export function bucketTier(b: Bucket | undefined, expected: number, isCurrent: boolean): Tier {
  if (!b) return 'none';
  // A real incident shows through regardless of sampling.
  if (b.down > 0) return 'down';
  if (b.degraded > 0) return 'degraded';
  // A COMPLETED bucket with fewer samples than the probe cadence expects is a monitoring data gap,
  // not a clean run — draw it grey ('partial') rather than a false green (the contract on
  // `expected_per_bucket` says exactly this). The in-progress current bucket is legitimately
  // mid-accumulation, so it is exempt (barTier fills it from live status).
  const total = b.ok + b.degraded + b.down;
  if (!isCurrent && expected > 0 && total < expected) return 'partial';
  if (b.ok > 0) return 'ok';
  return 'none';
}

/** The tier a live status draws, by the health rule the badge reads it by. */
const LIVE_TIER = { ok: 'ok', warn: 'degraded', bad: 'down' } as const;

/**
 * The tier a bar draws. The rightmost bar is the current, in-progress bucket, which has no sample
 * until the next probe tick (≤ 5 min during a run, up to an hour when idle), so it takes the LIVE component status rather than grey.
 * `liveStatus` is `null` when the latest probe failed (the status shown is the last good one, not
 * now's): then it stays grey. A status this UI does not know is not claimed healthy.
 */
export function barTier(
  b: Bucket | undefined,
  expected: number,
  isCurrent: boolean,
  liveStatus: string | null,
): Tier {
  const t = bucketTier(b, expected, isCurrent);
  if (t === 'none' && isCurrent && liveStatus !== null)
    return LIVE_TIER[healthOfStatus(liveStatus)];
  return t;
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
