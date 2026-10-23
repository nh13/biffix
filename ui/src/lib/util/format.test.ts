import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  errorMessage,
  fmtBytes,
  fmtDate,
  fmtRelative,
  fmtRelativeMicros,
  fmtUsd,
  fmtWallTime,
  plural,
} from './format';

const NOW = 1_700_000_000; // fixed "now" in epoch seconds

describe('errorMessage', () => {
  it("is an Error's message, without its class name", () => {
    class AuthRequiredError extends Error {}
    expect(errorMessage(new Error('boom'))).toBe('boom');
    expect(errorMessage(new AuthRequiredError('Your session has expired'))).toBe(
      'Your session has expired',
    );
  });
  it('is anything else as text', () => {
    expect(errorMessage('plain')).toBe('plain');
    expect(errorMessage(404)).toBe('404');
  });
});

describe('fmtDate', () => {
  it('renders the placeholder for a missing timestamp', () => {
    expect(fmtDate(null)).toBe('--');
    expect(fmtDate(undefined)).toBe('--');
  });
  it('formats a real timestamp (locale-dependent, so just assert it renders the year)', () => {
    // 2023-11-14 in UTC; the exact string is locale/timezone dependent, but the year must appear.
    expect(fmtDate(NOW)).toContain('2023');
  });
});

describe('fmtRelative', () => {
  beforeEach(() => vi.useFakeTimers({ now: NOW * 1000 }));
  afterEach(() => vi.useRealTimers());
  const ago = (secs: number) => fmtRelative(NOW - secs);

  it('renders each bucket up to its boundary', () => {
    expect(fmtRelative(null)).toBe('--');
    expect(ago(0)).toBe('just now');
    expect(ago(59)).toBe('just now');
    expect(ago(60)).toBe('1m ago');
    expect(ago(3599)).toBe('59m ago');
    expect(ago(3600)).toBe('1h ago');
    expect(ago(86_399)).toBe('23h ago');
    expect(ago(86_400)).toBe('1d ago');
    expect(ago(29 * 86_400)).toBe('29d ago');
    expect(ago(30 * 86_400)).toBe('1mo ago');
  });

  it('caps the month bucket at 11 and switches to years at 365d (no "0y ago" gap)', () => {
    expect(ago(359 * 86_400)).toBe('11mo ago');
    expect(ago(360 * 86_400)).toBe('11mo ago'); // regression: used to read "0y ago"
    expect(ago(364 * 86_400)).toBe('11mo ago');
    expect(ago(365 * 86_400)).toBe('1y ago');
    expect(ago(800 * 86_400)).toBe('2y ago');
  });

  it('says it in words, against a given now, on the same buckets', () => {
    const words = (secs: number) => fmtRelative(NOW - secs, { now: NOW, words: true });
    expect(words(5)).toBe('just now');
    expect(fmtRelative(NOW + 30, { now: NOW, words: true })).toBe('just now');
    expect(words(5 * 60)).toBe('5 min ago');
    expect(words(2 * 3600)).toBe('2 h ago');
    expect(words(30 * 3600)).toBe('yesterday');
    expect(words(4 * 86_400)).toBe('4 days ago');
    expect(words(29 * 86_400)).toBe('29 days ago');
    expect(words(30 * 86_400)).toBe('1 month ago');
    expect(words(90 * 86_400)).toBe('3 months ago');
    expect(words(364 * 86_400)).toBe('11 months ago'); // never "12 months ago"
    expect(words(365 * 86_400)).toBe('1 year ago');
    expect(words(800 * 86_400)).toBe('2 years ago');
  });

  it('reads microsecond timestamps too', () => {
    expect(fmtRelativeMicros(null)).toBe('--');
    expect(fmtRelativeMicros((NOW - 3600) * 1_000_000)).toBe('1h ago');
  });
});

describe('fmtWallTime', () => {
  it('is the placeholder without a submit time', () => {
    expect(fmtWallTime(null, null)).toBe('--');
    expect(fmtWallTime(undefined, 100)).toBe('--');
  });
  it('renders compact d/h/m spans, omitting leading zero units', () => {
    expect(fmtWallTime(0, 45 * 60)).toBe('45m');
    expect(fmtWallTime(0, 2 * 3600 + 14 * 60)).toBe('2h 14m');
    expect(fmtWallTime(0, 2 * 86_400 + 3 * 3600 + 5 * 60)).toBe('2d 3h 5m');
  });
  it('clamps a completed-before-submitted span to zero rather than going negative', () => {
    expect(fmtWallTime(100, 50)).toBe('0m');
  });
  it('measures against now for a still-running span', () => {
    vi.useFakeTimers({ now: NOW * 1000 });
    expect(fmtWallTime(NOW - 90 * 60, null)).toBe('1h 30m');
    vi.useRealTimers();
  });
});

describe('fmtUsd', () => {
  it('shows cents below $100, whole dollars above, and never a misleading $0.00', () => {
    expect(fmtUsd(0.4249)).toBe('$0.42');
    expect(fmtUsd(12.5)).toBe('$12.50');
    expect(fmtUsd(1234.4)).toBe('$1,234');
    expect(fmtUsd(0.001)).toBe('<$0.01');
    expect(fmtUsd(0)).toBe('$0.00');
    expect(fmtUsd(null)).toBe('--');
  });
  it('picks the precision after rounding to cents, at the $100 boundary', () => {
    expect(fmtUsd(99.99)).toBe('$99.99');
    expect(fmtUsd(99.994)).toBe('$99.99');
    expect(fmtUsd(99.995)).toBe('$100'); // rounds to $100.00, so reads like $100
    expect(fmtUsd(100)).toBe('$100');
    expect(fmtUsd(100.4)).toBe('$100');
    expect(fmtUsd(100.5)).toBe('$101');
  });
  it('rounds to zero only for amounts under half a cent', () => {
    expect(fmtUsd(0.004)).toBe('<$0.01');
    expect(fmtUsd(0.005)).toBe('$0.01');
    expect(fmtUsd(0.01)).toBe('$0.01');
  });
  it('puts a negative sign ahead of the $, and never shows -$0.00', () => {
    expect(fmtUsd(-12.5)).toBe('-$12.50');
    expect(fmtUsd(-1234.4)).toBe('-$1,234');
    expect(fmtUsd(-0.001)).toBe('$0.00');
    expect(fmtUsd(-0)).toBe('$0.00');
  });
});

describe('fmtBytes', () => {
  it('formats in binary units, with one decimal below 10', () => {
    expect(fmtBytes(0)).toBe('0 B');
    expect(fmtBytes(1023)).toBe('1023 B');
    expect(fmtBytes(1536)).toBe('1.5 KiB');
    expect(fmtBytes(20 * 1024 * 1024)).toBe('20 MiB');
    expect(fmtBytes(3 * 1024 ** 4)).toBe('3.0 TiB');
  });
  it('chooses the unit and precision after rounding, at every boundary', () => {
    const KiB = 1024;
    const MiB = 1024 ** 2;
    // B → KiB
    expect(fmtBytes(KiB - 1)).toBe('1023 B');
    expect(fmtBytes(KiB)).toBe('1.0 KiB');
    // one decimal → whole number at 10 (as rounded)
    expect(fmtBytes(9.94 * KiB)).toBe('9.9 KiB');
    expect(fmtBytes(9.96 * KiB)).toBe('10 KiB');
    expect(fmtBytes(10 * KiB - 1)).toBe('10 KiB');
    expect(fmtBytes(10 * KiB)).toBe('10 KiB');
    // KiB → MiB, and each larger unit, when the rounded value reaches 1024
    expect(fmtBytes(1023.4 * KiB)).toBe('1023 KiB');
    expect(fmtBytes(1023.6 * KiB)).toBe('1.0 MiB');
    expect(fmtBytes(MiB - 1)).toBe('1.0 MiB');
    expect(fmtBytes(MiB)).toBe('1.0 MiB');
    expect(fmtBytes(1024 ** 3 - 1)).toBe('1.0 GiB');
    expect(fmtBytes(1024 ** 4 - 1)).toBe('1.0 TiB');
    expect(fmtBytes(1024 ** 5 - 1)).toBe('1.0 PiB');
    // PiB is the largest unit: no carry past it (and never an undefined unit)
    expect(fmtBytes(1023.6 * 1024 ** 5)).toBe('1024 PiB');
    expect(fmtBytes(4096 * 1024 ** 5)).toBe('4096 PiB');
  });
  it('shows unknown sizes as --', () => {
    expect(fmtBytes(null)).toBe('--');
    expect(fmtBytes(undefined)).toBe('--');
    expect(fmtBytes(-1)).toBe('--');
  });
});

describe('plural', () => {
  it('counts a noun, with an odd plural when given', () => {
    expect(plural(1, 'sample')).toBe('1 sample');
    expect(plural(0, 'sample')).toBe('0 samples');
    expect(plural(1204, 'row')).toBe(`${(1204).toLocaleString()} rows`);
    expect(plural(2, 'child', 'children')).toBe('2 children');
  });
});
