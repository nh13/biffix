// The Usage page's pure logic: named periods as `[from, to)` dates, the page's
// URL state, and the CSV a viewer can download. Dates are UTC `YYYY-MM-DD`; `to` is exclusive.

import type { ReadonlyURLSearchParams } from '$app/state';
import { validParam } from '#lib/util/urlParam.js';

export type UsageGroupBy = 'month' | 'workflow' | 'submitter' | 'instance_type' | 'project';
export type PeriodPreset = 'this_month' | 'last_month' | 'last_3_months' | 'this_year' | 'custom';

export const GROUP_LABELS: Record<UsageGroupBy, string> = {
  month: 'Month',
  workflow: 'Workflow',
  submitter: 'Submitted by',
  instance_type: 'Instance type',
  project: 'Project',
};

export const PERIOD_LABELS: Record<PeriodPreset, string> = {
  this_month: 'This month',
  last_month: 'Last month',
  last_3_months: 'Last 3 months',
  this_year: 'This year',
  custom: 'Custom…',
};

const iso = (y: number, m: number) => new Date(Date.UTC(y, m, 1)).toISOString().slice(0, 10);

/** The `YYYY-MM-DD` date `days` after `date`. */
function addDays(date: string, days: number): string {
  const d = new Date(`${date}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
}

/** A named period's `[from, to)` at `now`; months are calendar months, UTC. */
export function presetRange(
  preset: Exclude<PeriodPreset, 'custom'>,
  now: Date,
): {
  from: string;
  to: string;
} {
  const y = now.getUTCFullYear();
  const m = now.getUTCMonth();
  switch (preset) {
    case 'this_month':
      return { from: iso(y, m), to: iso(y, m + 1) };
    case 'last_month':
      return { from: iso(y, m - 1), to: iso(y, m) };
    case 'last_3_months':
      return { from: iso(y, m - 2), to: iso(y, m + 1) };
    case 'this_year':
      return { from: iso(y, 0), to: iso(y + 1, 0) };
  }
}

/** The day before an exclusive `to`, for showing a period as "from – last day". */
export function lastDay(to: string): string {
  return addDays(to, -1);
}

/** The page's state as it lives in the URL. */
export interface UsageState {
  period: PeriodPreset;
  /** Only for `custom`. */
  from: string;
  to: string;
  groupBy: UsageGroupBy;
}

/**
 * A real calendar date as `YYYY-MM-DD`: `2026-13-01` and `2026-02-30` are not (a hand-edited URL
 * must not reach date arithmetic, which throws on the one and rolls the other over). Its year is
 * one the server takes (1970–9999), with room for the day after it (the exclusive end of a range).
 */
export function isDate(s: string | null): s is string {
  const m = s ? /^(\d{4})-(\d{2})-(\d{2})$/.exec(s) : null;
  if (!m) return false;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (y < 1970 || y > 9998) return false;
  const date = new Date(Date.UTC(y, mo - 1, d));
  return date.getUTCFullYear() === y && date.getUTCMonth() === mo - 1 && date.getUTCDate() === d;
}

const PERIODS = Object.keys(PERIOD_LABELS) as PeriodPreset[];
const GROUPS = Object.keys(GROUP_LABELS) as UsageGroupBy[];

export function parseUsageState(params: ReadonlyURLSearchParams): UsageState {
  const from = params.get('from');
  const to = params.get('to');
  return {
    period: validParam(params.get('period'), PERIODS, 'this_month'),
    from: isDate(from) ? from : '',
    to: isDate(to) ? to : '',
    groupBy: validParam(params.get('group_by'), GROUPS, 'month'),
  };
}

export function usageParams(state: UsageState): URLSearchParams {
  const p = new URLSearchParams();
  if (state.period !== 'this_month') p.set('period', state.period);
  if (state.period === 'custom') {
    if (state.from) p.set('from', state.from);
    if (state.to) p.set('to', state.to);
  }
  if (state.groupBy !== 'month') p.set('group_by', state.groupBy);
  return p;
}

/** The API's `[from, to)` for a state (custom: the dates given, `to` made exclusive). */
export function rangeFor(state: UsageState, now: Date): { from: string; to: string } | null {
  if (state.period !== 'custom') return presetRange(state.period, now);
  if (!state.from || !state.to) return null;
  return { from: state.from, to: addDays(state.to, 1) };
}

export interface UsageRowLike {
  key: string;
  runs: number;
  compute_usd: number;
  ebs_usd: number;
  total_usd: number;
}

/**
 * A CSV of the groups and the total (RFC 4180: quoting, and CRLF line ends). A key starting with
 * `=`, `+`, `-`, `@`, a tab or a carriage return gets a leading `'`, so a spreadsheet shows it as
 * text rather than running it as a formula (keys are names people choose: workflows, submitters).
 */
export function usageCsv(groupBy: UsageGroupBy, rows: UsageRowLike[], total: UsageRowLike): string {
  const quote = (raw: string) => {
    const s = /^[=+\-@\t\r]/.test(raw) ? `'${raw}` : raw;
    return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const usd = (n: number) => n.toFixed(2);
  const line = (r: UsageRowLike, key: string) =>
    [quote(key), r.runs, usd(r.compute_usd), usd(r.ebs_usd), usd(r.total_usd)].join(',');
  return (
    [
      `${groupBy},runs,compute_usd,storage_usd,total_usd`,
      ...rows.map((r) => line(r, r.key)),
      line(total, 'total'),
    ].join('\r\n') + '\r\n'
  );
}
