// Browser upload routes (uploads.rs): multipart uploads by id, with parts from 1 MiB so a small test
// file has several. The parts go to /mock-s3 (handlers/s3.mjs, standing in for S3's presigned
// URLs), which answers slowly enough for a test to catch an upload in flight.
import { DATA_OBJECTS } from '../fixtures/data.mjs';
import { missingField, readJson } from '../lib/http.mjs';
import { ORIGIN, UPLOADS } from '../state.mjs';
import { requestProject } from './data.mjs';

const MIB = 1024 * 1024;
let uploadSeq = 0;
// uploads.rs: at most 100 part URLs per request, part numbers 1..=10,000 (S3's limit), and an
// object of at most 5 TiB.
const MAX_PART_URLS = 100;
const MAX_PARTS = 10_000;
const MAX_OBJECT = 5 * 1024 * 1024 * MIB;
/** uploads.rs `plan_parts`, with a 1 MiB floor (the backend's is 8 MiB) so a small test file has
 * several parts: whole MiB, large enough that the file needs at most 10,000 parts. */
function planParts(size) {
  const part = Math.max(MIB, Math.ceil(Math.ceil(size / MAX_PARTS) / MIB) * MIB);
  return { part_size: part, parts: Math.max(1, Math.ceil(size / part)) };
}
const uploadUri = (bucket, key) => `s3://${bucket}/${key}`;
const taken = (key) => `an object already exists at ${key}; choose another name`;
/** uploads.rs `check_key`: the message for a key an upload may not use, else `null`. */
function checkKey(key) {
  const bad = (why) => `upload key \`${key}\` ${why}`;
  if (!key.startsWith('uploads/')) return bad('must be under uploads/');
  const rest = key.slice('uploads/'.length);
  if (Buffer.byteLength(key) > 1024) return bad('is longer than 1024 bytes');
  // Rust's `char::is_control`: the C0 and C1 control characters.
  if (/[\u0000-\u001f\u007f-\u009f]/.test(key)) return bad('contains a control character');
  if (!rest || rest.endsWith('/')) return bad('names a folder, not a file');
  if (rest.split('/').some((s) => s === '' || s === '.' || s === '..'))
    return bad('has an empty, `.` or `..` path segment');
  return null;
}

// uploads.rs `upload_uri`: the project's bucket, when browsers may upload into it.
function uploadBucket(named) {
  const { project, error } = requestProject(named);
  if (error) return { status: 400, error };
  if (!project.uploads)
    return { status: 403, error: "browser uploads are not enabled for this project's bucket" };
  return { bucket: project.bucket };
}

/** `POST /data/uploads`: start a multipart upload. */
function startUpload({ req, send }) {
  readJson(req, send, ({ key, size, project }) => {
    if (typeof key !== 'string') return send(422, missingField('key'));
    // `UploadStartRequest.size` is a required u64.
    if (size === undefined) return send(422, missingField('size'));
    if (!Number.isSafeInteger(size) || size < 0)
      return send(422, `size: invalid value: ${JSON.stringify(size)}, expected u64`);
    const target = uploadBucket(project);
    if (target.error) return send(target.status, target.error);
    const bad = checkKey(key);
    if (bad) return send(400, bad);
    if (size > MAX_OBJECT) return send(400, 'a file may be at most 5 TiB');
    const uri = uploadUri(target.bucket, key);
    // Like the backend: an upload never replaces an object.
    if (DATA_OBJECTS.some((o) => o.uri === uri)) return send(409, taken(key));
    const upload_id = `mock-upload-${++uploadSeq}`;
    UPLOADS.set(upload_id, { key, size, bucket: target.bucket, parts: new Map() });
    send(201, { upload_id, key, uri, ...planParts(size) });
  });
}

/**
 * An upload call on `{upload_id}`: every one names the upload's object key in its JSON body. The
 * backend checks the key, then asks S3 for that key's upload, so an upload asked for under another
 * key is not found. `cb` gets the upload and the body.
 */
const onUpload =
  (cb) =>
  ({ req, params, send }) =>
    readJson(req, send, (body) => {
      const { key, project } = body;
      if (typeof key !== 'string') return send(422, missingField('key'));
      const target = uploadBucket(project);
      if (target.error) return send(target.status, target.error);
      const bad = checkKey(key);
      if (bad) return send(400, bad);
      // S3 is asked for the upload in the named project's bucket: another bucket has no such one.
      const id = params.upload_id;
      const upload = UPLOADS.get(id);
      if (!upload || upload.key !== key || upload.bucket !== target.bucket)
        return send(404, 'no such upload');
      cb({ id, upload, body, send });
    });

/** `POST /data/uploads/{upload_id}/parts`: presigned URLs for some parts. */
const uploadParts = onUpload(({ id, body: { part_numbers }, send }) => {
  if (!Array.isArray(part_numbers)) return send(422, missingField('part_numbers'));
  if (part_numbers.length === 0 || part_numbers.length > MAX_PART_URLS)
    return send(400, `ask for 1–${MAX_PART_URLS} parts at a time`);
  const outOfRange = part_numbers.find((n) => !(n >= 1 && n <= MAX_PARTS));
  if (outOfRange !== undefined)
    return send(400, `part number ${outOfRange} is not in 1–${MAX_PARTS}`);
  send(200, {
    urls: part_numbers.map((n) => ({ part_number: n, url: `${ORIGIN}/mock-s3/${id}/${n}` })),
  });
});

/** `POST /data/uploads/{upload_id}/complete`: the object, once every part is in. */
const completeUpload = onUpload(({ id, upload, body: { parts }, send }) => {
  if (!Array.isArray(parts)) return send(422, missingField('parts'));
  const sorted = [...parts].sort((a, b) => a.part_number - b.part_number);
  if (!sorted.length || sorted.some((p, i) => p.part_number !== i + 1))
    return send(400, 'parts must be numbered 1, 2, 3… with none missing');
  // S3 refuses a part it does not have, or one whose ETag differs.
  const bad = sorted.find(
    (p) => !upload.parts.has(p.part_number) || upload.parts.get(p.part_number).etag !== p.etag,
  );
  if (bad) return send(400, `InvalidPart: part ${bad.part_number}`);
  // Create-only, as S3's conditional complete: another upload reached the key first. Like S3, a
  // refused complete leaves the upload open.
  const uri = uploadUri(upload.bucket, upload.key);
  if (DATA_OBJECTS.some((o) => o.uri === uri)) return send(409, taken(upload.key));
  UPLOADS.delete(id);
  // S3 assembles the listed parts only.
  const size = sorted.reduce((n, p) => n + upload.parts.get(p.part_number).size, 0);
  DATA_OBJECTS.push({ uri, size });
  send(200, { uri, etag: `mock-${parts.length}` });
});

/** `POST /data/uploads/{upload_id}/abort`. */
const abortUpload = onUpload(({ id, send }) => {
  UPLOADS.delete(id);
  send(204);
});

/** `POST /data/uploads/{upload_id}/status`: the parts S3 has. */
const uploadStatus = onUpload(({ upload, send }) =>
  send(200, {
    parts: [...upload.parts.entries()].map(([part_number, p]) => ({
      part_number,
      size: p.size,
      etag: p.etag,
    })),
  }),
);

export const UPLOAD_ROUTES = [
  ['POST', '/data/uploads', startUpload],
  ['POST', '/data/uploads/{upload_id}/parts', uploadParts],
  ['POST', '/data/uploads/{upload_id}/complete', completeUpload],
  ['POST', '/data/uploads/{upload_id}/abort', abortUpload],
  ['POST', '/data/uploads/{upload_id}/status', uploadStatus],
];
