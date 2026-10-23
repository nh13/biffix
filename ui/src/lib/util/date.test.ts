import { afterAll, describe, expect, it } from 'vitest';
import { localDate } from './date';

// CI runs in UTC, where a local date and a UTC date are the same thing, so pin zones where they
// differ; each case also checks `toISOString` disagrees, or it could not tell the two apart.
describe('localDate', () => {
  const saved = process.env.TZ;
  const inZone = (tz: string) => {
    process.env.TZ = tz;
  };
  afterAll(() => {
    if (saved === undefined) delete process.env.TZ;
    else process.env.TZ = saved;
  });

  it('formats the local calendar date west of UTC, zero-padded', () => {
    inZone('America/New_York');
    const lateEvening = new Date(2026, 0, 5, 23, 59); // already Jan 6 in UTC
    expect(lateEvening.toISOString().slice(0, 10)).toBe('2026-01-06');
    expect(localDate(lateEvening)).toBe('2026-01-05');
  });

  it('formats the local calendar date east of UTC', () => {
    inZone('Pacific/Kiritimati'); // UTC+14
    const earlyMorning = new Date(2026, 11, 31, 0, 1); // still Dec 30 in UTC
    expect(earlyMorning.toISOString().slice(0, 10)).toBe('2026-12-30');
    expect(localDate(earlyMorning)).toBe('2026-12-31');
  });
});
