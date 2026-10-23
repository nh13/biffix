import { describe, expect, it } from 'vitest';
import { describeEstimate, estimateLine, fmtEstimateUsd, fmtHours, typicalLine } from './estimate';

describe('the launch estimate', () => {
  it('says a range, a typical time and its basis', () => {
    expect(
      describeEstimate(
        { usd_low: 12.2, usd_high: 18.4, usd_median: 15, hours_median: 3, runs: 5 },
        6,
      ),
    ).toBe('About $12–$18 for 6 samples · usually ~3 h · from 5 runs');
  });
  it('keeps small amounts and short runs readable', () => {
    expect(
      describeEstimate(
        { usd_low: 0.4, usd_high: 0.4, usd_median: 0.4, hours_median: 0.25, runs: 2 },
        1,
      ),
    ).toBe('About $0.40 for 1 sample · usually ~15 min · from 2 runs');
    expect(
      describeEstimate(
        { usd_low: 3.25, usd_high: 1250, usd_median: 9, hours_median: 12.4, runs: 20 },
        40,
      ),
    ).toBe('About $3.3–$1,250 for 40 samples · usually ~12 h · from 20 runs');
  });
  it('picks the precision and unit after rounding', () => {
    expect(fmtEstimateUsd(0.996)).toBe('$1');
    expect(fmtEstimateUsd(9.96)).toBe('$10');
    expect(fmtEstimateUsd(99.6)).toBe('$100');
    expect(fmtEstimateUsd(0.004)).toBe('<$0.01');
    expect(fmtEstimateUsd(0)).toBe('$0.00');
    expect(fmtHours(0.995)).toBe('1 h');
    expect(fmtHours(9.96)).toBe('10 h');
    expect(fmtHours(0.5)).toBe('30 min');
  });
});

describe("the wizard's estimate line", () => {
  const none = { text: null, estimating: false, failed: false, samples: 3 };
  it('says the estimate, or why there is none', () => {
    expect(estimateLine({ ...none, text: 'About $12 for 3 samples' })).toBe(
      'About $12 for 3 samples. An estimate, not a limit.',
    );
    expect(estimateLine({ ...none, estimating: true })).toBe('Estimating the cost…');
    expect(estimateLine({ ...none, failed: true })).toBe("Couldn't get a cost estimate just now.");
    expect(estimateLine({ ...none, samples: 0 })).toBe(
      'No cost estimate: one is worked out from the samples listed on Add samples.',
    );
    expect(estimateLine(none)).toBe(
      'No cost estimate yet: it appears once this pipeline has finished analyses of samples to go by.',
    );
  });
});

describe('typicalLine', () => {
  it('says the usual time, the usual cost when priced, and how many runs it is from', () => {
    expect(typicalLine({ hours_median: 2.5, usd_median: 14.2, runs: 6 })).toBe(
      'Usually ~2.5 h · about $14 · from 6 analyses',
    );
    expect(typicalLine({ hours_median: 0.5, runs: 1 })).toBe('Usually ~30 min · from 1 analysis');
  });

  it('is nothing without a typical run', () => {
    expect(typicalLine(null)).toBeNull();
    expect(typicalLine(undefined)).toBeNull();
    expect(typicalLine({ hours_median: 0, runs: 3 })).toBeNull();
  });
});
