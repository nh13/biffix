import { describe, expect, it } from 'vitest';

import type { StatusHistory } from '#lib/api/index.js';
import {
  MAX_BUCKETS,
  barTier,
  bucketTier,
  bucketTimeline,
  historyByComponent,
  uptime,
} from './history';

const hist = (over: Partial<StatusHistory> = {}): StatusHistory => ({
  bucket_secs: 3600,
  window_secs: 4 * 3600,
  expected_per_bucket: 12,
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

describe('bucketTier', () => {
  it('is none without a bucket', () => {
    expect(bucketTier(undefined, 12, false)).toBe('none');
  });

  it('ranks down over degraded over ok', () => {
    expect(bucketTier({ ok: 10, degraded: 1, down: 1 }, 12, false)).toBe('down');
    expect(bucketTier({ ok: 11, degraded: 1, down: 0 }, 12, false)).toBe('degraded');
    expect(bucketTier({ ok: 12, degraded: 0, down: 0 }, 12, false)).toBe('ok');
  });

  it('draws a finished bucket with too few samples as partial, not green', () => {
    expect(bucketTier({ ok: 5, degraded: 0, down: 0 }, 12, false)).toBe('partial');
  });

  it('does not mark the in-progress bucket partial', () => {
    expect(bucketTier({ ok: 5, degraded: 0, down: 0 }, 12, true)).toBe('ok');
  });

  it('shows an incident even in an under-sampled bucket', () => {
    expect(bucketTier({ ok: 0, degraded: 0, down: 1 }, 12, false)).toBe('down');
  });
});

describe('barTier', () => {
  it('fills an empty current bucket from the live status', () => {
    expect(barTier(undefined, 12, true, 'down')).toBe('down');
    expect(barTier(undefined, 12, true, 'degraded')).toBe('degraded');
    expect(barTier(undefined, 12, true, 'ok')).toBe('ok');
  });

  it('draws no data for the current bucket while the live status is stale or unknown', () => {
    // The probe failed: the last good status is not now's.
    expect(barTier(undefined, 12, true, null)).toBe('none');
    // A status this UI does not know is not claimed healthy.
    expect(barTier(undefined, 12, true, 'maintenance')).toBe('degraded');
  });

  it('leaves an empty past bucket as no data', () => {
    expect(barTier(undefined, 12, false, 'down')).toBe('none');
  });

  it('keeps a sampled current bucket as sampled', () => {
    expect(barTier({ ok: 1, degraded: 0, down: 0 }, 12, true, 'down')).toBe('ok');
  });
});

describe('historyByComponent and uptime', () => {
  const h = hist({
    components: [
      {
        name: 'api',
        buckets: [
          { t: 0, ok: 9, degraded: 1, down: 0 },
          { t: 3600, ok: 10, degraded: 0, down: 0 },
        ],
      },
      { name: 'database', buckets: [{ t: 0, ok: 0, degraded: 0, down: 0 }] },
    ],
  });
  const map = historyByComponent(h);

  it('keys each component by bucket start', () => {
    expect(map.get('api')?.get(3600)).toEqual({ ok: 10, degraded: 0, down: 0 });
  });

  it('counts degraded samples against uptime', () => {
    expect(uptime(map.get('api'))).toBeCloseTo(95);
  });

  it('is null with no samples or no history', () => {
    expect(uptime(map.get('database'))).toBeNull();
    expect(uptime(map.get('missing'))).toBeNull();
  });
});
