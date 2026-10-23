// The backend's CSV writing (csv.rs) and its downloads' headers, for an experiment's samples CSV.

/** csv.rs `needs_guard`: a value that starts with a tab, CR, LF or `'`, or whose first character
 * after leading whitespace starts a formula (`= + - @` or their full-width forms). */
const CSV_GUARD = /^[\t\r\n']|^[ \t\r\n]*[=+\-@＝＋－＠]/u;
/** csv.rs `field`: a value that could run as a formula gets a leading `'`, then RFC 4180 quoting. */
export function csvField(raw) {
  const s = CSV_GUARD.test(raw) ? `'${raw}` : raw;
  return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}
/** csv.rs `json_escaped_len`: how many bytes `text` takes as a JSON string's contents. */
export const jsonEscapedLen = (text) => Buffer.byteLength(JSON.stringify(text)) - 2;
/** runs/samples_csv.rs `csv_time`: epoch seconds as `YYYY-MM-DD HH:MM:SS` (UTC), or empty. */
export const csvTime = (secs) =>
  secs == null
    ? ''
    : new Date(Math.floor(secs) * 1000).toISOString().slice(0, 19).replace('T', ' ');
/** object_view.rs `content_disposition`: a plain `filename=` (quotes, backslashes and control
 * characters dropped, each other non-ASCII character `_`; `download` when blank), plus an RFC 5987
 * `filename*=` with the exact UTF-8 name when it is not plain ASCII, every byte but `A-Za-z0-9-._~`
 * percent-encoded (as `urlencoding::encode` does). */
export function contentDisposition(kind, filename) {
  let ascii = [...filename]
    .filter((c) => !/\p{Cc}/u.test(c) && c !== '"' && c !== '\\')
    .map((c) => (c.codePointAt(0) < 0x80 ? c : '_'))
    .join('');
  if (ascii.trim() === '') ascii = 'download';
  if (/^[\x00-\x7f]*$/.test(filename) && ascii === filename) return `${kind}; filename="${ascii}"`;
  const encoded = [...Buffer.from(filename, 'utf8')]
    .map((b) => {
      const c = String.fromCharCode(b);
      return /[A-Za-z0-9\-._~]/.test(c) ? c : `%${b.toString(16).toUpperCase().padStart(2, '0')}`;
    })
    .join('');
  return `${kind}; filename="${ascii}"; filename*=UTF-8''${encoded}`;
}
