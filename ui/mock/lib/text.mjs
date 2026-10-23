// The backend's checks on people's text (biffix-workflow text.rs, formschema/catalog.rs).

/** text.rs `EMAIL_RULE`: what `plausibleEmail` accepts, for the message that turns one away. */
export const EMAIL_RULE =
  'one `@`, a dotted domain, no spaces, and none of `? & # % , < > =` (unsafe in a mail link)';
/** text.rs `plausible_email`. */
export function plausibleEmail(email) {
  const at = email.indexOf('@');
  if (at < 0) return false;
  const local = email.slice(0, at);
  const domain = email.slice(at + 1);
  return (
    Buffer.byteLength(email) <= 254 &&
    local !== '' &&
    !domain.includes('@') &&
    domain.includes('.') &&
    !domain.startsWith('.') &&
    !domain.endsWith('.') &&
    !/[\s\p{Cc}?&#%,<>=]/u.test(email)
  );
}
/** text.rs `is_http_url`: an absolute http(s) URL with a host and no spaces. */
export function isHttpUrl(url) {
  const rest = /^https?:\/\/(.*)$/i.exec(url)?.[1];
  if (rest == null || rest.startsWith('/') || /[\s\p{Cc}]/u.test(url)) return false;
  try {
    return new URL(url).hostname !== '';
  } catch {
    return false;
  }
}

// formschema/catalog.rs: the longest contact name and help-page URL, in characters.
const MAX_CONTACT_NAME_CHARS = 100;
const MAX_CONTACT_URL_CHARS = 500;
/** catalog.rs `normalize_contact`: a contact trimmed, or `{ field, why }` for the first problem. */
export function normalizeContact(c) {
  const name = String(c.name ?? '').trim();
  if (!name || [...name].length > MAX_CONTACT_NAME_CHARS)
    return { field: 'name', why: `must be 1 to ${MAX_CONTACT_NAME_CHARS} characters` };
  const email = String(c.email ?? '').trim();
  if (!plausibleEmail(email))
    return { field: 'email', why: `\`${email}\` is not a valid email address (${EMAIL_RULE})` };
  const url = String(c.url ?? '').trim();
  if (url) {
    if ([...url].length > MAX_CONTACT_URL_CHARS)
      return { field: 'url', why: `must be at most ${MAX_CONTACT_URL_CHARS} characters` };
    if (!isHttpUrl(url)) return { field: 'url', why: 'must be an http:// or https:// URL' };
  }
  return { contact: { name, email, ...(url ? { url } : {}) } };
}
