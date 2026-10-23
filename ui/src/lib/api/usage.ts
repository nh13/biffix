// Estimated spend (the usage page).

import { API_PATHS, get, withQuery, type JsonBody } from './client';

export type UsageReport = JsonBody<'get_usage'>;

/** Estimated spend of the runs submitted in `[from, to)`, grouped. */
export function getUsage(from: string, to: string, groupBy: string): Promise<UsageReport> {
  return get<UsageReport>(
    withQuery(API_PATHS.usage, new URLSearchParams({ from, to, group_by: groupBy })),
  );
}
