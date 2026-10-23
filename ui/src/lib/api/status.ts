// The deployment's health (the status page).

import { API_PATHS, get, withQuery, type JsonBody } from './client';

export type StatusReport = JsonBody<'status'>;
export type StatusHistory = JsonBody<'status_history'>;

export function getStatus(): Promise<StatusReport> {
  return get<StatusReport>(API_PATHS.status);
}

export type StatusWindow = '1h' | '1d' | '1w' | '90d';

export function getStatusHistory(window: StatusWindow = '90d'): Promise<StatusHistory> {
  return get<StatusHistory>(withQuery(API_PATHS.statusHistory, new URLSearchParams({ window })));
}
