import { describe, expect, it } from 'vitest';
import {
  isDate,
  lastDay,
  parseUsageState,
  presetRange,
  rangeFor,
  usageCsv,
  usageParams,
} from './usage';

const now = new Date('2026-01-15T12:00:00Z');

describe('periods', () => {
  it('names calendar ranges, crossing year boundaries', () => {
    expect(presetRange('this_month', now)).toEqual({ from: '2026-01-01', to: '2026-02-01' });
    expect(presetRange('last_month', now)).toEqual({ from: '2025-12-01', to: '2026-01-01' });
    expect(presetRange('last_3_months', now)).toEqual({ from: '2025-11-01', to: '2026-02-01' });
    expect(presetRange('this_year', now)).toEqual({ from: '2026-01-01', to: '2027-01-01' });
    expect(lastDay('2026-03-01')).toBe('2026-02-28');
  });

  it('makes a custom range end-inclusive for the API, and needs both dates', () => {
    const custom = {
      period: 'custom' as const,
      from: '2026-01-01',
      to: '2026-01-31',
      groupBy: 'month' as const,
    };
    expect(rangeFor(custom, now)).toEqual({ from: '2026-01-01', to: '2026-02-01' });
    expect(rangeFor({ ...custom, to: '' }, now)).toBeNull();
  });
});

describe('URL state', () => {
  it('round-trips, leaving defaults out', () => {
    const state = {
      period: 'custom' as const,
      from: '2026-01-01',
      to: '2026-01-31',
      groupBy: 'workflow' as const,
    };
    expect(parseUsageState(usageParams(state))).toEqual(state);
    expect(usageParams(parseUsageState(new URLSearchParams())).toString()).toBe('');
  });
  it('ignores values it does not know', () => {
    const s = parseUsageState(new URLSearchParams('period=forever&group_by=team&from=soon'));
    expect(s).toEqual({ period: 'this_month', from: '', to: '', groupBy: 'month' });
    // Names every object has (from its prototype) are not values either.
    for (const q of [
      'period=toString&group_by=constructor',
      'period=constructor&group_by=toString',
      'period=__proto__&group_by=hasOwnProperty',
    ])
      expect(parseUsageState(new URLSearchParams(q))).toEqual({
        period: 'this_month',
        from: '',
        to: '',
        groupBy: 'month',
      });
  });
  it('drops dates that are not on the calendar, so the range never throws or rolls over', () => {
    for (const bad of ['2026-13-01', '2026-00-10', '2026-01-32', '2026-02-30', '2026-13-45']) {
      const s = parseUsageState(new URLSearchParams(`period=custom&from=2026-01-01&to=${bad}`));
      expect(s.to).toBe('');
      expect(rangeFor(s, new Date('2026-09-28T00:00:00Z'))).toBeNull();
    }
    expect(isDate('2024-02-29')).toBe(true);
    expect(isDate('2026-02-29')).toBe(false);
    // Years the server refuses (its range ends the day after `to`, so 9999 is out too).
    for (const far of ['1969-12-31', '9999-12-31', '0001-01-01'])
      expect(isDate(far), far).toBe(false);
  });
});

describe('usageCsv', () => {
  it('writes the groups then the total, quoting keys that need it', () => {
    const row = (key: string, total: number) => ({
      key,
      runs: 2,
      compute_usd: total - 0.5,
      ebs_usd: 0.5,
      total_usd: total,
    });
    expect(
      usageCsv('submitter', [row('Doe, Jane', 3), row('a"b', 1.234)], row('total', 4.234)),
    ).toBe(
      'submitter,runs,compute_usd,storage_usd,total_usd\r\n' +
        '"Doe, Jane",2,2.50,0.50,3.00\r\n' +
        '"a""b",2,0.73,0.50,1.23\r\n' +
        'total,2,3.73,0.50,4.23\r\n',
    );
  });
  it('writes a key that a spreadsheet would run as a formula as text', () => {
    const row = (key: string) => ({ key, runs: 1, compute_usd: 1, ebs_usd: 0, total_usd: 1 });
    const csv = usageCsv(
      'workflow',
      [row('=1+2'), row('+x'), row('-x'), row('@SUM(A1)'), row('=HYPERLINK("http://x","a")')],
      row('total'),
    );
    expect(csv.split('\r\n').slice(1, 6)).toEqual([
      "'=1+2,1,1.00,0.00,1.00",
      "'+x,1,1.00,0.00,1.00",
      "'-x,1,1.00,0.00,1.00",
      "'@SUM(A1),1,1.00,0.00,1.00",
      '"\'=HYPERLINK(""http://x"",""a"")",1,1.00,0.00,1.00',
    ]);
  });
});
