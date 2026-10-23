import { describe, expect, it } from 'vitest';
import { nextPage, pageRange, pastLastPage, prevPage } from './runsPager';

describe('runs keyset paging', () => {
  it('goes forward after the last cursor and back before the first', () => {
    expect(nextPage(0, 'c50')).toEqual({ index: 1, req: { after: 'c50' } });
    expect(prevPage(2, 'c101')).toEqual({ index: 1, req: { before: 'c101' } });
  });

  it('asks for the true first page (no cursor) when Prev reaches it', () => {
    // A `before` cursor on page one would pin it, so newer runs never appear.
    expect(prevPage(1, 'c51')).toEqual({ index: 0, req: {} });
    expect(prevPage(3, undefined)).toEqual({ index: 0, req: {} });
  });

  it('has no next page without a next cursor (never page one under a later index)', () => {
    expect(nextPage(2, undefined)).toBeNull();
  });

  it('notices a page left past the end by a shrinking total', () => {
    expect(pastLastPage(0, 50, 0)).toBe(false);
    expect(pastLastPage(1, 50, 51)).toBe(false);
    expect(pastLastPage(1, 50, 50)).toBe(true);
    expect(pastLastPage(2, 50, 30)).toBe(true);
    expect(pastLastPage(1, 50, 0)).toBe(true);
  });

  it('shows a range that is never reversed', () => {
    expect(pageRange(0, 50, 0)).toEqual({ start: 0, end: 0 });
    expect(pageRange(0, 50, 30)).toEqual({ start: 1, end: 30 });
    expect(pageRange(1, 50, 120)).toEqual({ start: 51, end: 100 });
    expect(pageRange(2, 50, 30)).toEqual({ start: 30, end: 30 });
  });
});
