import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { StatusReport } from '#lib/api/index.js';

const getStatus = vi.fn<() => Promise<StatusReport>>();
vi.mock('#lib/api/index.js', () => ({ getStatus: () => getStatus() }));

const { healthOf, liveStatus, watchStatus, STATUS_POLL_MS } = await import('./live.svelte');

const report = (status: StatusReport['status']): StatusReport => ({ status, components: [] });
const flush = () => vi.advanceTimersByTimeAsync(0);

describe('healthOf', () => {
  it('is bad when the probe failed, whatever the last report said', () => {
    expect(healthOf(report('ok'), 'unreachable')).toBe('bad');
  });
  it('maps the report status, and is unknown before any report', () => {
    expect(healthOf(null, null)).toBe('unknown');
    expect(healthOf(report('ok'), null)).toBe('ok');
    expect(healthOf(report('degraded'), null)).toBe('warn');
    expect(healthOf(report('down'), null)).toBe('bad');
  });
});

describe('watchStatus', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    getStatus.mockReset();
    // The shared state outlives a test: each starts as a page would, with nothing probed yet.
    liveStatus.report = null;
    liveStatus.error = null;
    liveStatus.settled = false;
  });
  afterEach(() => vi.useRealTimers());

  it('runs one poll however many watch it, and stops when the last releases', async () => {
    getStatus.mockResolvedValue(report('ok'));
    const a = watchStatus();
    const b = watchStatus();
    await flush();
    expect(getStatus).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(STATUS_POLL_MS);
    expect(getStatus).toHaveBeenCalledTimes(2);

    a();
    a(); // a second release is a no-op, not a release of b
    await vi.advanceTimersByTimeAsync(STATUS_POLL_MS);
    expect(getStatus).toHaveBeenCalledTimes(3);

    b();
    await vi.advanceTimersByTimeAsync(STATUS_POLL_MS * 3);
    expect(getStatus).toHaveBeenCalledTimes(3);
  });

  it('reports a failure after a good probe, keeping the last report', async () => {
    getStatus.mockResolvedValueOnce(report('ok')).mockRejectedValueOnce(new Error('offline'));
    const release = watchStatus();
    await flush();
    expect(liveStatus.report?.status).toBe('ok');
    expect(liveStatus.error).toBeNull();
    expect(liveStatus.settled).toBe(true);

    await vi.advanceTimersByTimeAsync(STATUS_POLL_MS);
    expect(liveStatus.error).toBe('offline');
    expect(liveStatus.report?.status).toBe('ok');

    getStatus.mockResolvedValueOnce(report('degraded'));
    await vi.advanceTimersByTimeAsync(STATUS_POLL_MS);
    expect(liveStatus.error).toBeNull();
    expect(liveStatus.report?.status).toBe('degraded');
    release();
  });
});
