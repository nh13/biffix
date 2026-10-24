import { describe, expect, it } from 'vitest';
import { REFERENCE_FIELD } from '../referenceChoice';
import { stepToFix } from './fieldFocus';

const placed = { samples: new Set(['input']), review: new Set(['outdir']) };

describe('stepToFix', () => {
  it('never moves the Expert form', () => {
    expect(stepToFix('outdir', { wizard: false, step: 2, placed })).toBeNull();
  });

  it("moves the wizard to the step a field is on, and stays on that field's own step", () => {
    const on = (step: 1 | 2 | 3 | 4) => ({ wizard: true, step, placed });
    expect(stepToFix('input', on(4))).toBe(2);
    expect(stepToFix('outdir', on(2))).toBe(4);
    expect(stepToFix('min_reads', on(2))).toBe(3);
    expect(stepToFix(REFERENCE_FIELD, on(4))).toBe(3);
    expect(stepToFix('input', on(2))).toBeNull();
    expect(stepToFix('min_reads', on(3))).toBeNull();
  });
});
