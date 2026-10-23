// The site's settings routes (settings.rs): its title, logo and support contact, and the
// operational-alert recipients.
import { ALERTS, SETTINGS } from '../fixtures/settings.mjs';
import { missingField, readJson } from '../lib/http.mjs';
import { bytewise } from '../lib/paging.mjs';
import { EMAIL_RULE, normalizeContact, plausibleEmail } from '../lib/text.mjs';

/** `GET /settings/alerts`. */
function getAlerts({ send }) {
  send(200, ALERTS);
}

/** The address a recipient request names, trimmed, or `null` once it answered axum's 422. */
function emailOf(body, send) {
  if (typeof body.email === 'string') return body.email.trim();
  send(422, missingField('email'));
  return null;
}
const sameEmail = (a, b) => a.toLowerCase() === b.toLowerCase();

/** `POST /settings/alerts`: add a recipient (pending until confirmed), as settings.rs does: a
 * plausible address, trimmed; one already there (in any case) is a no-op. */
function addAlert({ req, send }) {
  readJson(req, send, (body) => {
    const email = emailOf(body, send);
    if (email == null) return;
    if (!plausibleEmail(email))
      return send(400, `not a valid email address: ${JSON.stringify(email)} (${EMAIL_RULE})`);
    if (!ALERTS.recipients.some((r) => sameEmail(r.email, email))) {
      ALERTS.recipients.push({ email, status: 'pending' });
      ALERTS.recipients.sort((a, b) => bytewise(a.email.toLowerCase(), b.email.toLowerCase()));
    }
    send(200, ALERTS);
  });
}

/** `POST /settings/alerts/remove`: the address in the body, out of the URL. Only a confirmed
 * recipient can be removed (SNS has no id for a pending one), as settings.rs answers. */
function removeAlert({ req, send }) {
  readJson(req, send, (body) => {
    const email = emailOf(body, send);
    if (email == null) return;
    const matching = ALERTS.recipients.filter((r) => sameEmail(r.email, email));
    if (!matching.length) return send(404, `${email} is not a recipient`);
    if (!matching.some((r) => r.status === 'confirmed'))
      return send(
        409,
        `${email} has not confirmed its subscription yet, so it cannot be removed; an unconfirmed subscription lapses on its own after 3 days`,
      );
    ALERTS.recipients = ALERTS.recipients.filter((r) => !sameEmail(r.email, email));
    send(204);
  });
}

/** `GET /settings`. */
function getSettings({ send }) {
  send(200, SETTINGS);
}

// settings.rs: the title cap (characters), the logo cap (bytes of the data URL) and logo types.
const MAX_TITLE_CHARS = 100;
const MAX_LOGO_BYTES = 512 * 1024;
const LOGO_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
// settings.rs `looks_like`: the first bytes of an image of each type.
const LOOKS_LIKE = {
  'image/png': (b) => b.subarray(0, 8).equals(Buffer.from('\x89PNG\r\n\x1a\n', 'latin1')),
  'image/jpeg': (b) => b.subarray(0, 3).equals(Buffer.from([0xff, 0xd8, 0xff])),
  'image/gif': (b) => ['GIF87a', 'GIF89a'].includes(b.subarray(0, 6).toString('latin1')),
  'image/webp': (b) =>
    b.subarray(0, 4).toString('latin1') === 'RIFF' &&
    b.subarray(8, 12).toString('latin1') === 'WEBP',
};
/** settings.rs `check_logo`: the 400's message for a logo it refuses, else `null`. */
function logoProblem(logo) {
  if (logo.length > MAX_LOGO_BYTES)
    return `logo too large (${logo.length} bytes; max ${MAX_LOGO_BYTES})`;
  const comma = logo.indexOf(',');
  if (!logo.startsWith('data:') || comma < 0) return 'logo must be a data: URL';
  const [mediaType, ...params] = logo.slice('data:'.length, comma).split(';');
  const type = mediaType.trim().toLowerCase();
  if (!LOGO_TYPES.includes(type))
    return `logo must be one of ${LOGO_TYPES.join(', ')} (got \`${type}\`)`;
  if (!params.some((p) => p.trim().toLowerCase() === 'base64'))
    return 'logo must be a base64 data: URL';
  const data = logo.slice(comma + 1).trim();
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(data) || data.length % 4 !== 0)
    return "logo's data is not valid base64";
  return LOOKS_LIKE[type](Buffer.from(data, 'base64'))
    ? null
    : `logo's data is not a ${type} image`;
}

/** `PUT /settings`: a partial update, like the backend: an absent field keeps, null or blank
 * clears. Every field is checked before any is written, so a refused update changes nothing. */
function putSettings({ req, send }) {
  readJson(req, send, (u) => {
    const title = 'title' in u ? (u.title ?? '').trim() || null : undefined;
    if (title && [...title].length > MAX_TITLE_CHARS)
      return send(400, `title must be at most ${MAX_TITLE_CHARS} characters`);
    const logo = 'logo_data_url' in u ? u.logo_data_url || null : undefined;
    const bad = logo ? logoProblem(logo) : null;
    if (bad) return send(400, bad);
    // As settings.rs checks it (catalog.rs `normalize_contact`); `null` clears it.
    let support;
    if (u.support != null) {
      const { contact, field, why } = normalizeContact(u.support);
      if (!contact) return send(400, `support.${field}: ${why}`);
      support = contact;
    }
    if (title !== undefined) SETTINGS.title = title;
    if (logo !== undefined) SETTINGS.logo_data_url = logo;
    if ('support' in u) {
      if (support) SETTINGS.support = support;
      else delete SETTINGS.support;
    }
    send(200, SETTINGS);
  });
}

export const SETTINGS_ROUTES = [
  ['GET', '/settings', getSettings],
  ['PUT', '/settings', putSettings],
  ['GET', '/settings/alerts', getAlerts],
  ['POST', '/settings/alerts', addAlert],
  ['POST', '/settings/alerts/remove', removeAlert],
];
