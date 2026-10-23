// A sortable table column's indicators, shared by the list pages so the glyph and the
// accessibility state can't drift between them.

export type SortDir = 'asc' | 'desc';

/** The arrow after a column's label: none unless the table is sorted by it. */
export function sortArrow<K>(key: K, sortKey: K | null, sortDir: SortDir): string {
  return sortKey !== key ? '' : sortDir === 'asc' ? ' ↑' : ' ↓';
}

/** The column header's `aria-sort`, so a screen reader announces the sort, not just the glyph. */
export function ariaSort<K>(
  key: K,
  sortKey: K | null,
  sortDir: SortDir,
): 'ascending' | 'descending' | 'none' {
  return sortKey !== key ? 'none' : sortDir === 'asc' ? 'ascending' : 'descending';
}
