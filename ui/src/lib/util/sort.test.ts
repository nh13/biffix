import { describe, expect, it } from 'vitest';
import { ariaSort, sortArrow } from './sort';

describe('sort indicators', () => {
  it('mark only the sorted column, in its direction', () => {
    expect(sortArrow('a', null, 'asc')).toBe('');
    expect(sortArrow('a', 'b', 'asc')).toBe('');
    expect(sortArrow('a', 'a', 'asc')).toBe(' ↑');
    expect(sortArrow('a', 'a', 'desc')).toBe(' ↓');
    expect(ariaSort('a', null, 'desc')).toBe('none');
    expect(ariaSort('a', 'b', 'asc')).toBe('none');
    expect(ariaSort('a', 'a', 'asc')).toBe('ascending');
    expect(ariaSort('a', 'a', 'desc')).toBe('descending');
  });
});
