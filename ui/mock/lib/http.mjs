// Request bodies, read as axum reads them. One mock serves every e2e worker, so a body a handler
// cannot take answers that one request and never ends the process: a throw in a body's callback is
// answered with `status` (axum refuses a JSON body of the wrong shape with 422) and logged.

/** Run `cb`, answering a throw with `status` and the error's message. */
export function guarded(send, status, prefix, cb) {
  try {
    cb();
  } catch (e) {
    console.error(e);
    send(status, `${prefix}${e instanceof Error ? e.message : String(e)}`);
  }
}

/** A request's raw body; a stream error, or a throw in `cb`, is a 500. */
export function readBody(req, send, cb) {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('error', (e) => send(500, `the request body could not be read: ${e.message}`));
  req.on('end', () => guarded(send, 500, '', () => cb(Buffer.concat(chunks))));
}
// axum's prefix for a JSON body it parsed but could not read into the handler's type (a 422).
export const DESERIALIZE = 'Failed to deserialize the JSON body into the target type: ';
/** Whether a request says its body is JSON, as axum's `Json` requires (`application/json`, or a
 * `+json` type). */
const saysJson = (req) =>
  /^application\/(?:[\w.-]+\+)?json\s*(?:;|$)/i.test(req.headers['content-type'] ?? '');
/**
 * A request's JSON body, as axum's `Json` reads it: one not labelled JSON is a 415, an empty one
 * or one that is not JSON a 400, and one that is not an object a 422; a handler that throws on a
 * field of the wrong type is a 422 too.
 */
export function readJson(req, send, cb) {
  if (!saysJson(req)) {
    req.resume();
    return send(415, 'Expected request with `Content-Type: application/json`');
  }
  readBody(req, send, (b) => {
    let body;
    try {
      body = JSON.parse(b.toString());
    } catch (e) {
      return send(400, `Failed to parse the request body as JSON: ${e.message}`);
    }
    if (body === null || typeof body !== 'object' || Array.isArray(body)) {
      const kind = body === null ? 'null' : Array.isArray(body) ? 'a sequence' : typeof body;
      return send(422, `${DESERIALIZE}invalid type: ${kind}, expected a JSON object`);
    }
    guarded(send, 422, DESERIALIZE, () => cb(body));
  });
}
/** A request's body as JSON, or `null` when it is not JSON (the caller words the 400). */
export function readJsonOrNull(req, send, cb) {
  readBody(req, send, (raw) => {
    let body;
    try {
      body = JSON.parse(raw.toString());
    } catch {
      body = null;
    }
    guarded(send, 422, DESERIALIZE, () => cb(body));
  });
}
/** A JSON body's fields as parameters (nulls left out). */
export function jsonToParams(body) {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(body ?? {})) if (v != null) params.set(k, String(v));
  return params;
}
// axum's answer (422) for a JSON body without a required field.
export const missingField = (name) => `${DESERIALIZE}missing field \`${name}\``;
