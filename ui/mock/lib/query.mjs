// Typed request parameters, read as axum's `Query` (serde_urlencoded) and `Json` read them: a value
// of the wrong type is refused (a query string's 400, a POST form's body's 422), never read as
// absent or as a default.

import { DESERIALIZE } from './http.mjs';

/** The refusal of parameter `name`'s value, as `[status, message]`. */
export const refusal = (name, fromBody, why) =>
  fromBody
    ? [422, `${DESERIALIZE}${name}: ${why}`]
    : [400, `Failed to deserialize query string: ${name}: ${why}`];

/** An optional boolean: `{ value }` (absent: `false`), or `{ error: [status, message] }`. */
export function boolParam(params, name, fromBody = false) {
  const raw = params.get(name);
  if (raw == null) return { value: false };
  if (raw === 'true' || raw === 'false') return { value: raw === 'true' };
  return { error: refusal(name, fromBody, 'provided string was not `true` or `false`') };
}

/** An optional whole number of at least `min`: `{ value }` (absent: `dflt`), or `{ error }`. */
export function intParam(params, name, dflt, fromBody = false, min = -Infinity) {
  const raw = params.get(name);
  if (raw == null) return { value: dflt };
  if (!/^-?\d+$/.test(raw))
    return { error: refusal(name, fromBody, 'invalid digit found in string') };
  const value = Number(raw);
  if (value < min)
    return { error: refusal(name, fromBody, `invalid value: ${raw}, expected ${min} or more`) };
  return { value };
}
