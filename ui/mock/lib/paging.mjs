// The order and the page sizes the backend's listings share.
import { refusal } from './query.mjs';

// Rust's `str` order (bytewise), not the locale's: the order every backend listing uses. (JS
// compares UTF-16 code units, which differ from UTF-8 bytes only past the BMP.)
export const bytewise = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
/**
 * A page size as the backend reads one: absent is `dflt`, else clamped to lo..=hi; `null` for one
 * that is not an integer (blank, `abc`, `2.5`), which the backend's typed `limit` refuses
 * (`badLimit`).
 */
export function clampInt(raw, dflt, lo, hi) {
  if (raw == null) return dflt;
  const text = String(raw);
  if (!/^-?\d+$/.test(text)) return null;
  return Math.min(Math.max(Number(text), lo), hi);
}
/** The refusal of a `limit` that is not an integer, as `[status, message]`: axum's `Query` answers
 * 400, its `Json` (a POST form's body) 422. */
export const badLimit = (raw, fromBody) =>
  refusal(
    'limit',
    fromBody,
    fromBody ? `invalid value: \`${raw}\`, expected i64` : 'invalid digit found in string',
  );
