// Calendar dates as the user sees them.

/** `d`'s date in the browser's time zone, as `YYYY-MM-DD` (not UTC, as `toISOString` gives). */
export function localDate(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
