// Small display helpers shared across the monitor.

export function fmtDate(epochSecs?: number | null): string {
  if (epochSecs == null) return '--';
  return new Date(epochSecs * 1000).toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/** How `fmtRelative` words a time: compact (`3h ago`, the tables') or in words (`3 h ago`). */
export interface RelativeOptions {
  /** "Now", in epoch seconds (default: this browser's clock); fixed in tests. */
  now?: number;
  /**
   * In words, for a sentence or a notification: `5 min ago`, `2 h ago`, `yesterday`, `4 days
   * ago`, `3 months ago`, `1 year ago`. The buckets are the compact form's.
   */
  words?: boolean;
}

// Relative time, e.g. "just now", "5m ago", "3h ago", "2d ago", "4mo ago" (or in words, see
// `RelativeOptions`). Uses floored buckets so a value never reads "60m ago" (it becomes "1h ago" at
// the boundary). A time ahead of `now` (a clock ahead of this browser's) is "just now".
export function fmtRelative(epochSecs?: number | null, opts: RelativeOptions = {}): string {
  if (epochSecs == null) return '--';
  const { now = Date.now() / 1000, words = false } = opts;
  const secs = Math.floor(now - epochSecs);
  if (secs < 60) return 'just now';
  const m = Math.floor(secs / 60);
  if (m < 60) return words ? `${m} min ago` : `${m}m ago`;
  const h = Math.floor(secs / 3600);
  if (h < 24) return words ? `${h} h ago` : `${h}h ago`;
  const d = Math.floor(secs / 86400);
  if (d < 30) return words ? (d === 1 ? 'yesterday' : `${d} days ago`) : `${d}d ago`;
  // Switch to years at 365 days, not at 12 month-buckets (the ÷30 and ÷365 buckets don't meet):
  // months cap at 11 so 330–364d reads "11mo ago" and 365d is exactly "1y ago", never "0y ago".
  if (d < 365) {
    const mo = Math.min(Math.floor(d / 30), 11);
    return words ? `${plural(mo, 'month')} ago` : `${mo}mo ago`;
  }
  const y = Math.floor(d / 365);
  return words ? `${plural(y, 'year')} ago` : `${y}y ago`;
}

/** `n` and the noun for it: `1 sample`, `12 samples`, `1,204 rows` (`many` for an odd plural). */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`;
}

// Same, for epoch-**microsecond** timestamps (e.g. workflow_versions.created_at).
export function fmtRelativeMicros(micros?: number | null): string {
  return micros == null ? '--' : fmtRelative(Math.floor(micros / 1_000_000));
}

// Wall time between submit and completion (or now, if still running). Compact "2h 14m" form.
export function fmtWallTime(submitted?: number | null, completed?: number | null): string {
  if (submitted == null) return '--';
  const end = completed ?? Math.floor(Date.now() / 1000);
  let secs = Math.max(0, end - submitted);
  const d = Math.floor(secs / 86400);
  secs -= d * 86400;
  const h = Math.floor(secs / 3600);
  secs -= h * 3600;
  const m = Math.floor(secs / 60);
  const parts: string[] = [];
  if (d) parts.push(`${d}d`);
  if (h) parts.push(`${h}h`);
  parts.push(`${m}m`);
  return parts.join(' ');
}

/**
 * A USD amount for cost estimates: cents below $100 (`$0.42`), whole dollars from $100 (`$1,234`),
 * and `<$0.01` for a positive amount that would round to zero. The precision is chosen from the
 * amount as rounded to cents, so $99.995 reads `$100` like $100 itself. A negative amount keeps its
 * sign ahead of the `$` (`-$12.50`); one that rounds to zero reads `$0.00`.
 */
export function fmtUsd(usd?: number | null): string {
  if (usd == null || !Number.isFinite(usd)) return '--';
  const abs = Math.abs(usd);
  const money = (digits: number) =>
    abs.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits });
  // Round the way the text will (Intl, not float arithmetic), then pick the precision.
  const cents = Number(money(2).replace(/,/g, ''));
  if (cents === 0) return usd > 0 ? '<$0.01' : '$0.00';
  return `${usd < 0 ? '-' : ''}$${money(cents >= 100 ? 0 : 2)}`;
}

/**
 * A byte count in binary units (`1.5 MiB`); `--` when unknown. One decimal below 10, whole numbers
 * from 10. The unit and precision are chosen after rounding, so 1023.6 KiB reads `1.0 MiB` (not
 * `1024 KiB`) and 9.96 KiB reads `10 KiB` (not `10.0 KiB`).
 */
export function fmtBytes(bytes?: number | null): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return '--';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  let value = bytes / 1024;
  let unit = 0;
  for (;;) {
    const tenths = Math.round(value * 10) / 10;
    if (tenths < 10) return `${tenths.toFixed(1)} ${units[unit]}`;
    const whole = Math.round(value);
    // The largest unit has nowhere to carry to: past 1023.5 PiB it just keeps counting.
    if (whole < 1024 || unit === units.length - 1) return `${whole} ${units[unit]}`;
    value /= 1024;
    unit += 1;
  }
}

/**
 * What to tell the user about a failure: an Error's own message, without the "Error: " (or
 * "AuthRequiredError: ") prefix `String(e)` would put in front of it.
 */
export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
