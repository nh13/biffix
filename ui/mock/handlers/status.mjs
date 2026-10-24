// The deploy's status routes (health.rs): the probe's report and its history.
import { STATUS } from '../fixtures/status.mjs';
import { wallClock } from '../lib/clock.mjs';

// `/status/history?window=`, sized per window as health.rs `window_spec` does (bucket = window / 90
// bars, at least the 5-minute probe cadence). The samples are the probe's, each promising the next
// within 5 minutes (a tick that saw a run) or the hour (an idle one), and a bucket says how far its
// newest sample's promise runs (`covered_until`) and whether a sample came later than the one
// before it promised (`lapsed`). event_ingest, degraded now, was degraded in its newest bucket and
// once before; reconciler has a gap (a late sample in a bucket with too few, or two 1-sample
// buckets missed and the next one late); alarms is sampled hourly, as on an idle tenant, so most of
// its short buckets have no sample yet are covered (its 1h window by the sample before it).
const HISTORY_WINDOWS = { '1h': 3_600, '1d': 86_400, '1w': 604_800, '90d': 7_776_000 };
const PROBE_CADENCE_SECS = 300;
const IDLE_SECS = 3_600;
const GRACE_SECS = 300;
function statusHistory(window) {
  // Own keys only: `toString` is not a window.
  if (!Object.hasOwn(HISTORY_WINDOWS, window)) return null;
  const window_secs = HISTORY_WINDOWS[window];
  const bucket_secs = Math.max(Math.floor(window_secs / 90), PROBE_CADENCE_SECS);
  const expected = Math.max(Math.floor(bucket_secs / PROBE_CADENCE_SECS), 1);
  // Bucketed from the request's time, as health.rs is.
  const latest = Math.floor(wallClock() / bucket_secs) * bucket_secs;
  const bars = Math.floor(window_secs / bucket_secs);
  const first = latest - (bars - 1) * bucket_secs;
  // A bucket's last tick, 30 s into its final five minutes, promising the next within five.
  const promised = (t) => ({ covered_until: t + bucket_secs + 30, lapsed: false, last: 'ok' });
  const sample = (name, i, t) => {
    const ingestDegraded = name === 'event_ingest' && (i === 0 || i === 5);
    if (ingestDegraded) {
      const degraded = Math.max(1, Math.floor(expected / 3));
      return { ok: expected - degraded, degraded, down: 0, ...promised(t), last: 'degraded' };
    }
    if (name === 'reconciler' && expected > 1 && i === 9)
      return { ok: Math.ceil(expected / 6), degraded: 0, down: 0, ...promised(t), lapsed: true };
    if (name === 'reconciler' && expected === 1 && (i === 9 || i === 8)) return null;
    if (name === 'reconciler' && expected === 1 && i === 7)
      return { ok: 1, degraded: 0, down: 0, ...promised(t), lapsed: true };
    if (name === 'alarms') {
      // The hour's samples, 30 s past each hour, that fall in the bucket.
      const hours = [];
      for (let h = Math.ceil((t - 30) / IDLE_SECS); h * IDLE_SECS + 30 < t + bucket_secs; h++)
        hours.push(h * IDLE_SECS + 30);
      if (hours.length === 0) return null;
      const covered_until = hours[hours.length - 1] + IDLE_SECS;
      return { ok: hours.length, degraded: 0, down: 0, covered_until, lapsed: false, last: 'ok' };
    }
    return { ok: expected, degraded: 0, down: 0, ...promised(t) };
  };
  // The alarms' hourly sample before the first bar, whose promise reaches into the window.
  const before = Math.floor((first - 30 - 1) / IDLE_SECS) * IDLE_SECS + 30;
  return {
    bucket_secs,
    window_secs,
    expected_per_bucket: expected,
    grace_secs: GRACE_SECS,
    components: STATUS.components.map((c) => ({
      name: c.name,
      buckets: Array.from({ length: bars }, (_, k) => bars - 1 - k)
        .map((i) => {
          const t = latest - i * bucket_secs;
          const s = sample(c.name, i, t);
          return s && { t, ...s };
        })
        .filter(Boolean),
      ...(c.name === 'alarms' && before + IDLE_SECS + GRACE_SECS > first
        ? { carried_in: { covered_until: before + IDLE_SECS, last: 'ok' } }
        : {}),
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
