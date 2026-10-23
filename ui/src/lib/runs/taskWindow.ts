// The Jobs table's loaded window of a run's tasks, fetched so that "Show more", the live poll and a
// filter change can never leave the table with a row twice or another filter's rows. (A window of
// several chunks is several reads of a live, re-sorting table, so a row moving between them can
// be missed by both, leaving the window a row short until the next refresh.)
//
// - The window is always re-fetched whole from offset 0 (never an offset page appended to rows
//   fetched earlier, which a live, re-sorting list shifts under), in server-sized chunks.
// - Only the newest request's answer is applied: a slower, older one (a filter since changed, or a
//   poll tick that started before "Show more") resolves to null instead.
// - Rows are unique by task key even across chunks (each chunk is its own read of a live table).

/**
 * The largest page the tasks API returns (`MAX_PAGE` in crates/biffix-backend/src/api/tasks.rs).
 */
export const MAX_TASK_PAGE = 1000;

/** `items` with only the first of each key, order kept: a keyed `{#each}` throws on a repeat. */
export function uniqueBy<T>(items: readonly T[], key: (item: T) => string): T[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    const k = key(item);
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

/**
 * `fetch`, but only the newest call's outcome counts: an older call that settles after a newer one
 * started resolves to `null` (its rejection too), so it cannot overwrite what the newer one shows.
 */
export function newestOnly<A extends unknown[], T>(
  fetch: (...args: A) => Promise<T>,
): (...args: A) => Promise<T | null> {
  let latest = 0;
  return async (...args: A) => {
    const id = ++latest;
    try {
      const value = await fetch(...args);
      return id === latest ? value : null;
    } catch (e) {
      if (id === latest) throw e;
      return null;
    }
  };
}

interface Page<T> {
  tasks: T[];
  total: number;
}

/**
 * The first `limit` rows, as one page: fetched in chunks of at most {@link MAX_TASK_PAGE} (all at
 * once), joined, and made unique by task key. Every other field (the total, the counts) is the
 * last chunk's, the newest read. `limit` is at least 1.
 */
export async function fetchWindow<T extends { task_key: string }, P extends Page<T>>(
  fetchPage: (offset: number, limit: number) => Promise<P>,
  limit: number,
): Promise<P> {
  const offsets: number[] = [];
  for (let offset = 0; offset < limit; offset += MAX_TASK_PAGE) offsets.push(offset);
  const pages = await Promise.all(
    offsets.map((offset) => fetchPage(offset, Math.min(MAX_TASK_PAGE, limit - offset))),
  );
  const last = pages[pages.length - 1];
  return {
    ...last,
    tasks: uniqueBy(
      pages.flatMap((p) => p.tasks),
      (t) => t.task_key,
    ),
  };
}
