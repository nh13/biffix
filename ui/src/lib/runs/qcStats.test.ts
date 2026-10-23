import { describe, expect, it } from 'vitest';
import { formatQcValue, qcStatus, sortSamples } from './qcStats';

describe('formatQcValue', () => {
  it('uses the column decimals and suffix, grouping digits', () => {
    expect(formatQcValue(43.04, { decimals: 1, suffix: '%' })).toBe('43.0%');
    expect(formatQcValue(1234567, { decimals: 0, suffix: ' bp' })).toBe('1,234,567 bp');
    expect(formatQcValue(2.1, { suffix: 'M' })).toBe('2.1M');
    expect(formatQcValue(0.031605, { suffix: 'M' })).toBe('0.0M');
    expect(formatQcValue(150, {})).toBe('150');
    expect(formatQcValue(null, { suffix: '%' })).toBe('');
  });
});

describe('qcStatus', () => {
  const rules = { pass: [{ gt: 30 }], warn: [{ le: 30 }], fail: [{ lt: 10 }] };
  it('checks pass, warn then fail, the last match winning', () => {
    expect(qcStatus(35, rules)).toBe('pass');
    expect(qcStatus(20, rules)).toBe('warn');
    expect(qcStatus(5, rules)).toBe('fail');
  });
  it('supports string and equality rules', () => {
    expect(qcStatus(3, { fail: [{ s_eq: '3' }] })).toBe('fail');
    expect(qcStatus(3, { warn: [{ ne: 3 }] })).toBeNull();
    expect(qcStatus(3, { warn: [{ eq: '3' }] })).toBe('warn');
  });
  it('matches a rule when any of its keys holds, as MultiQC does', () => {
    // MultiQC checks each key on its own: {gt: 10, lt: 20} matches everything.
    expect(qcStatus(15, { warn: [{ gt: 10, lt: 20 }] })).toBe('warn');
    expect(qcStatus(25, { warn: [{ gt: 10, lt: 20 }] })).toBe('warn');
    expect(qcStatus(5, { fail: [{ lt: 10, gt: 90 }] })).toBe('fail');
    expect(qcStatus(95, { fail: [{ lt: 10, gt: 90 }] })).toBe('fail');
    expect(qcStatus(50, { fail: [{ lt: 10, gt: 90 }] })).toBeNull();
  });
  it('compares strings without case, a whole number as an int or a float', () => {
    expect(qcStatus(1e-7, { fail: [{ s_eq: '1E-07' }] })).toBe('fail');
    expect(qcStatus(30, { fail: [{ s_eq: '30' }] })).toBe('fail');
    expect(qcStatus(30, { fail: [{ s_eq: '30.0' }] })).toBe('fail');
    expect(qcStatus(0.5, { warn: [{ s_contains: '.5' }] })).toBe('warn');
    expect(qcStatus(30, { warn: [{ s_ne: '30' }] })).toBeNull();
    expect(qcStatus(31, { warn: [{ s_ne: '30' }] })).toBe('warn');
  });
  it('stops a rule at a target that is not a number, keeping earlier matches', () => {
    expect(qcStatus(5, { fail: [{ gt: 'many', lt: 10 }] })).toBeNull();
    expect(qcStatus(5, { fail: [{ s_eq: '5', gt: 'many' }] })).toBe('fail');
  });
  it('is null without rules, a value, or a known operator', () => {
    expect(qcStatus(5, undefined)).toBeNull();
    expect(qcStatus(null, rules)).toBeNull();
    expect(qcStatus(5, { fail: [{ between: 1 }] })).toBeNull();
    expect(qcStatus(5, { fail: [{}] })).toBeNull();
  });
});

describe('sortSamples', () => {
  const samples = [
    { name: 'S10', values: { gc: 41 } },
    { name: 'S2', values: {} as Record<string, number> },
    { name: 'S1', values: { gc: 43 } },
  ];
  it('sorts by a column with missing values last, or by name naturally', () => {
    expect(sortSamples(samples, 'gc', false).map((s) => s.name)).toEqual(['S10', 'S1', 'S2']);
    expect(sortSamples(samples, 'gc', true).map((s) => s.name)).toEqual(['S1', 'S10', 'S2']);
    expect(sortSamples(samples, null, false).map((s) => s.name)).toEqual(['S1', 'S2', 'S10']);
  });
  it('orders the samples missing a column by name naturally, either way the column sorts', () => {
    const missing = [
      { name: 'S10', values: {} as Record<string, number> },
      { name: 'S2', values: {} as Record<string, number> },
      { name: 'S1', values: { gc: 1 } },
    ];
    expect(sortSamples(missing, 'gc', false).map((s) => s.name)).toEqual(['S1', 'S2', 'S10']);
    expect(sortSamples(missing, 'gc', true).map((s) => s.name)).toEqual(['S1', 'S2', 'S10']);
  });
});
