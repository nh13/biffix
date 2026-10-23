import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { RunDetail } from '$lib/api';
import { listCostLine } from '$lib/runs/cost';
import {
  cancelledBanner,
  costText,
  progressFraction,
  sectionOrder,
  shareSentence,
  startedLine,
} from './analysis';
import { fmtDate } from '$lib/util/format';

describe('sectionOrder', () => {
  it('leads with what matters for the state', () => {
    expect(sectionOrder('succeeded')).toEqual(['results', 'qc', 'samples', 'inputs', 'cost']);
    expect(sectionOrder('failed')).toEqual(['wrong', 'results', 'samples', 'inputs', 'cost']);
    expect(sectionOrder('cancelled')).toEqual(['results', 'samples', 'inputs', 'cost']);
    for (const live of ['running', 'submitted', 'preparing_images', 'something_new'])
      expect(sectionOrder(live)).toEqual(['samples', 'results', 'inputs', 'cost']);
  });
});

describe('progressFraction', () => {
  it('counts samples against a samplesheet, else steps against a plan', () => {
    expect(
      progressFraction({ samples: { total: 12, done: 9, failed: 0, declared: true }, total: 40 }),
    ).toBe(0.75);
    expect(
      progressFraction({
        samples: { total: 3, done: 1, failed: 0, declared: false },
        completed: 5,
        total: 10,
      }),
    ).toBe(0.5);
    expect(progressFraction({ completed: 3, total: null })).toBeNull();
    expect(progressFraction({ completed: 12, total: 10 })).toBe(1);
  });
});

describe('startedLine', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_000_000 * 1000));
  });
  afterEach(() => vi.useRealTimers());

  it('says who started it, "you" for the signed-in user', () => {
    const run = {
      submitted_at: 1_000_000 - 7200,
      submitter: 'user:x',
      submitter_label: 'priya@lab.example.org',
    };
    expect(startedLine(run, 'priya@lab.example.org')).toBe('Started 2h ago by you');
    expect(startedLine(run, 'sam@lab.example.org')).toBe('Started 2h ago by priya@lab.example.org');
    expect(startedLine({ ...run, submitter_label: null }, null)).toBe('Started 2h ago by x');
    expect(startedLine({ submitted_at: null }, null)).toBe('Started');
  });
});

describe('costText', () => {
  const cost = (estimated_usd: number, priced_jobs: number, pending_jobs = 0) => ({
    cost: { estimated_usd, priced_jobs, pending_jobs } as RunDetail['cost'],
  });
  it('is an estimate so far while live, the total once ended', () => {
    expect(costText(cost(14.2, 3, 1), true)).toEqual({
      amount: '≈ $14.20 so far',
      note: 'estimate',
    });
    expect(costText(cost(13.8, 5), false)?.amount).toBe('$13.80');
    expect(costText({ cost: null }, false)).toBeNull();
  });
  it('marks an ended run whose jobs are still to be priced', () => {
    const text = costText(cost(13.8, 5, 1), false);
    expect(text?.amount).toBe('$13.80 + pending');
    expect(text?.note).toMatch(/can still grow/);
  });
  it('has none while nothing is priced, as the lists show a dash', () => {
    expect(costText(cost(0, 0, 2), true)).toBeNull();
    expect(costText(cost(0, 0, 2), false)).toBeNull();
  });
  it('agrees with the lists on the same run', () => {
    for (const [usd, priced, pending] of [
      [13.8, 5, 0],
      [13.8, 5, 1],
      [0, 0, 2],
    ])
      for (const live of [true, false]) {
        const page = costText(cost(usd, priced, pending), live)?.amount ?? '—';
        const row = listCostLine({
          cost_usd: priced > 0 ? usd : null,
          cost_pending: pending > 0,
          terminal: !live,
        }).text;
        expect(row, `${usd}/${priced}/${pending} live=${live}`).toBe(page);
      }
  });
});

describe('cancelledBanner', () => {
  it('names who cancelled and when, when the server knows', () => {
    expect(
      cancelledBanner({ cancelled_by: 'priya@lab.example.org', completed_at: 1_800_000_000 }),
    ).toEqual({ label: 'Cancelled by priya@lab.example.org', detail: fmtDate(1_800_000_000) });
  });
  it('else says when it stopped', () => {
    expect(cancelledBanner({ cancelled_by: null, completed_at: 1_800_000_000 })).toEqual({
      label: 'Cancelled',
      detail: `stopped ${fmtDate(1_800_000_000)}`,
    });
    expect(cancelledBanner({ cancelled_by: ' ', completed_at: null })).toEqual({
      label: 'Cancelled',
      detail: '',
    });
  });
});

describe('shareSentence', () => {
  it('names the project whose members can open the page', () => {
    expect(shareSentence('genomics')).toBe(
      'Anyone signed in to biffix with access to project genomics can open it.',
    );
    expect(shareSentence(null)).toBe(
      'Anyone signed in to biffix with access to its project can open it.',
    );
  });
});
