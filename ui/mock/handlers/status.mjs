// The deploy's status routes (health.rs): the probe's report and its history.
import { STATUS } from '../fixtures/status.mjs';
import { wallClock } from '../lib/clock.mjs';

// `/status/history?window=`, sized per window as health.rs `window_spec` does (bucket = window / 90
// bars, at least the 5-minute probe cadence). The samples are the probe's: event_ingest, degraded
// now, was degraded in its newest bucket and once before; reconciler has a gap (a bucket with too
// few samples, or none at all in a 1-sample bucket).
const HISTORY_WINDOWS = { '1h': 3_600, '1d': 86_400, '1w': 604_800, '90d': 7_776_000 };
const PROBE_CADENCE_SECS = 300;
function statusHistory(window) {
  // Own keys only: `toString` is not a window.
  if (!Object.hasOwn(HISTORY_WINDOWS, window)) return null;
  const window_secs = HISTORY_WINDOWS[window];
  const bucket_secs = Math.max(Math.floor(window_secs / 90), PROBE_CADENCE_SECS);
  const expected = Math.max(Math.floor(bucket_secs / PROBE_CADENCE_SECS), 1);
  // Bucketed from the request's time, as health.rs is.
  const latest = Math.floor(wallClock() / bucket_secs) * bucket_secs;
  const bars = Math.floor(window_secs / bucket_secs);
  const sample = (name, i) => {
    const ingestDegraded = name === 'event_ingest' && (i === 0 || i === 5);
    if (ingestDegraded) {
      const degraded = Math.max(1, Math.floor(expected / 3));
      return { ok: expected - degraded, degraded, down: 0 };
    }
    if (name === 'reconciler' && i === 9)
      return expected > 1 ? { ok: Math.ceil(expected / 6), degraded: 0, down: 0 } : null;
    return { ok: expected, degraded: 0, down: 0 };
  };
  return {
    bucket_secs,
    window_secs,
    expected_per_bucket: expected,
    components: STATUS.components.map((c) => ({
      name: c.name,
      buckets: Array.from({ length: bars }, (_, k) => bars - 1 - k)
        .map((i) => {
          const s = sample(c.name, i);
          return s && { t: latest - i * bucket_secs, ...s };
        })
        .filter(Boolean),
    })),
  };
}

/** `GET /status`. */
function getStatus({ send }) {
  send(200, STATUS);
}

/** `GET /status/history?window=`. */
function getStatusHistory({ query, send }) {
  const window = query.get('window') ?? '90d';
  const history = statusHistory(window);
  return history
    ? send(200, history)
    : send(400, `unknown window \`${window}\` (1h, 1d, 1w or 90d)`);
}

export const STATUS_ROUTES = [
  ['GET', '/status', getStatus],
  ['GET', '/status/history', getStatusHistory],
];
