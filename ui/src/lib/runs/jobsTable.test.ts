import { describe, expect, it } from 'vitest';
import { nextJobIndex } from './jobsTable';

describe('nextJobIndex', () => {
  it('starts at the first row for j and the last for k', () => {
    expect(nextJobIndex(-1, 1, 5)).toBe(0);
    expect(nextJobIndex(-1, -1, 5)).toBe(4);
  });

  it('steps from the selected row and stops at either end', () => {
    expect(nextJobIndex(2, 1, 5)).toBe(3);
    expect(nextJobIndex(2, -1, 5)).toBe(1);
    expect(nextJobIndex(4, 1, 5)).toBe(4);
    expect(nextJobIndex(0, -1, 5)).toBe(0);
  });
});
