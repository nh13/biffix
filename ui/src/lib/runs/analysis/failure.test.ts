import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  causeWords,
  failureLine,
  groupByCause,
  triesText,
  wordedCauses,
  type FailureItem,
} from './failure';

const item = (over: Partial<FailureItem>): FailureItem => ({
  step: 'STAR_ALIGN',
  cause: 'out_of_memory',
  message: 'it ran out of memory',
  advice: 'Ask for more memory.',
  attempts: 1,
  log_tail: [],
  try_again: 'resume',
  ...over,
});

describe('failure wording', () => {
  it('names the sample, the step and what happened', () => {
    expect(failureLine(item({ sample: 'Tumor_4' }))).toBe(
      'Tumor_4 · STAR align · it ran out of memory',
    );
    expect(failureLine(item({ sample: null }))).toBe('STAR align · it ran out of memory');
    // The author's label wins.
    expect(failureLine(item({ sample: null }), { STAR_ALIGN: 'Align (STAR)' })).toBe(
      'Align (STAR) · it ran out of memory',
    );
  });

  it('counts tries only when there was more than one', () => {
    expect(triesText(2)).toBe('tried 2 times');
    expect(triesText(1)).toBe('');
    expect(triesText(0)).toBe('');
  });

  it('gives an unknown cause the unknown words', () => {
    expect(causeWords('cosmic_ray')).toBe(causeWords('unknown'));
    // Only its own causes: never an Object member.
    expect(causeWords('constructor')).toBe(causeWords('unknown'));
    expect(causeWords('toString')).toBe(causeWords('unknown'));
    expect(causeWords('out_of_memory')).toMatch(/memory/);
  });
});

describe('groupByCause', () => {
  it('keeps at most three failures, each cause together in first-seen order', () => {
    const items = [
      item({ sample: 'A' }),
      item({ sample: 'B', cause: 'bad_input' }),
      item({ sample: 'C' }),
      item({ sample: 'D' }),
    ];
    expect(groupByCause(items).map((g) => [g.cause, g.items.map((i) => i.sample)])).toEqual([
      ['out_of_memory', ['A', 'C']],
      ['bad_input', ['B']],
    ]);
    expect(groupByCause([])).toEqual([]);
  });
});

describe('causeWords', () => {
  // Written by `emit_schemas` from `FailureCause::ALL` (crates/biffix-core).
  const contract = JSON.parse(
    readFileSync(new URL('../../../../../schemas/failure-causes.json', import.meta.url), 'utf8'),
  ) as { causes: string[] };

  it('words every cause the server sends, and no cause it does not', () => {
    expect(contract.causes).toContain('unknown');
    for (const cause of contract.causes.filter((c) => c !== 'unknown'))
      expect(causeWords(cause), cause).not.toBe(causeWords('a_cause_no_one_sends'));
    expect([...wordedCauses()].sort()).toEqual([...contract.causes].sort());
  });

  it('words a failure before launch by the stage it stopped at', () => {
    expect(causeWords('launch_failed', 'image_preparation')).toBe(
      "biffix could not prepare the pipeline's software.",
    );
    expect(causeWords('launch_failed', 'launch')).toBe('biffix could not start the analysis.');
    expect(causeWords('launch_failed', null)).toBe('biffix could not start the analysis.');
  });
});
