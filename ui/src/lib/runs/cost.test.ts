import { describe, expect, it } from 'vitest';
import type { RunDetail } from '#lib/api/index.js';
import { costLine, jobCostText, listCostLine, noCostLine, pricedUsd } from './cost';

describe('pricedUsd', () => {
  it('is the estimate once a job is priced, else none', () => {
    const cost = (estimated_usd: number, priced_jobs: number) =>
      ({ estimated_usd, priced_jobs, pending_jobs: 1 }) as RunDetail['cost'];
    expect(pricedUsd(cost(13.8, 2))).toBe(13.8);
    expect(pricedUsd(cost(0, 0))).toBeNull();
    // No job priced is none, whatever the estimate; a priced estimate of zero is a cost of zero.
    expect(pricedUsd(cost(5, 0))).toBeNull();
    expect(pricedUsd(cost(0, 2))).toBe(0);
    expect(pricedUsd(null)).toBeNull();
    expect(pricedUsd(undefined)).toBeNull();
  });
});

describe('costLine', () => {
  it('reads so far while live, + pending once ended with jobs to price, else the total', () => {
    expect(costLine(14.2, { live: true, pending: true })).toEqual({
      text: '≈ $14.20 so far',
      title: 'An estimate: still running',
      sr: 'about $14.20 so far',
    });
    const ended = costLine(13.8, { live: false, pending: true });
    expect([ended.text, ended.sr]).toEqual([
      '$13.80 + pending',
      '$13.80 so far; some machines not priced yet',
    ]);
    expect(costLine(13.8, { live: false, pending: false }).text).toBe('$13.80');
    expect(costLine(1234.4, { live: false, pending: false }).text).toBe('$1,234');
    expect(costLine(0.001, { live: false, pending: false }).text).toBe('<$0.01');
  });
});

describe('noCostLine', () => {
  it('is a dash, read out as why: none yet, or none recorded once nothing can be priced', () => {
    expect(noCostLine({ live: true, pending: true })).toEqual({
      text: '—',
      title: 'No cost yet',
      sr: 'No cost yet',
    });
    expect(noCostLine({ live: false, pending: true }).sr).toBe('No cost yet');
    expect(noCostLine({ live: false, pending: false }).sr).toBe('No cost recorded');
  });
});

describe('listCostLine', () => {
  it('reads a list row as the run page reads the run', () => {
    expect(listCostLine({ cost_usd: 1.2, cost_pending: true, terminal: false }).text).toBe(
      '≈ $1.20 so far',
    );
    expect(listCostLine({ cost_usd: 13.8, cost_pending: true, terminal: true }).text).toBe(
      '$13.80 + pending',
    );
    expect(listCostLine({ cost_usd: 13.8, cost_pending: false, terminal: true }).text).toBe(
      '$13.80',
    );
    // Nothing priced yet, only pending jobs: a dash, as on the run page.
    expect(listCostLine({ cost_pending: true, terminal: false }).sr).toBe('No cost yet');
    expect(listCostLine({ cost_usd: null, terminal: true }).sr).toBe('No cost recorded');
  });
});

describe('jobCostText', () => {
  it('marks a cost an estimate, but not one below a cent', () => {
    expect(jobCostText(1.234)).toBe('~$1.23');
    expect(jobCostText(0.001)).toBe('<$0.01');
  });

  it('is blank when the job is not priced', () => {
    expect(jobCostText(null)).toBe('');
    expect(jobCostText(undefined)).toBe('');
  });
});
