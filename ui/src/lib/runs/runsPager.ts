// The Runs list's keyset paging, as plain arithmetic so it unit-tests without a page. The server
// orders runs newest-submitted first (unsubmitted runs first of all); a page is asked for by the
// cursor that bounds it, and `index` counts pages only for the "X–Y of Z" range.

/** The cursor request for one page: empty for the first page (the newest runs). */
export interface PageRequest {
  after?: string;
  before?: string;
}

export interface PagerPosition {
  index: number;
  req: PageRequest;
}

/**
 * The page before `index`. Back on the first page the request carries no cursor: a `before` cursor
 * pins the page to the runs next to it, so runs submitted since would never show there (and Prev
 * is disabled on the first page, so they could not be reached either).
 */
export function prevPage(index: number, firstCursor: string | undefined): PagerPosition {
  const prev = index - 1;
  return prev <= 0 || !firstCursor
    ? { index: 0, req: {} }
    : { index: prev, req: { before: firstCursor } };
}

/**
 * The page after `index`; null without a cursor to read it from (an empty request would load the
 * first page under this page's "X–Y" range).
 */
export function nextPage(index: number, lastCursor: string | undefined): PagerPosition | null {
  return lastCursor ? { index: index + 1, req: { after: lastCursor } } : null;
}

/**
 * Whether page `index` now lies past the last page (live polling shrank the total under it, e.g. an
 * `active` filter as runs finish): the list should go back to the first page, or the range would
 * read a reversed "101–30 of 30" (or "No runs" with runs still on page one).
 */
export function pastLastPage(index: number, limit: number, total: number): boolean {
  return index > 0 && index * limit >= total;
}

/** The 1-based range of runs page `index` shows, clamped so it never reads reversed. */
export function pageRange(
  index: number,
  limit: number,
  total: number,
): { start: number; end: number } {
  const end = Math.min((index + 1) * limit, total);
  const start = total === 0 ? 0 : Math.min(index * limit + 1, end);
  return { start, end };
}
