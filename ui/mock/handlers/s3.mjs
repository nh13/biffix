// S3 stand-ins, outside the API: the alignment view's files (`/mock-files/igv/*`, served as a
// presigned GET is) and the upload parts' presigned PUTs (`/mock-s3/{upload_id}/{part}`).
import { readFileSync } from 'node:fs';
import { FIXTURES } from '../lib/backend-fixtures.mjs';
import { readBody } from '../lib/http.mjs';
import { UPLOADS } from '../state.mjs';

// How long S3 takes over a part: slow enough for a test to catch an upload in flight.
const PART_DELAY_MS = 250;

// The tiny BAM, BAI, FASTA and FAI every mock alignment view links to (the backend's fixtures).
const MOCK_FILES = Object.fromEntries(
  ['tiny.bam', 'tiny.bam.bai', 'tiny.fa', 'tiny.fa.fai'].map((n) => [
    n,
    readFileSync(new URL(`igv/${n}`, FIXTURES)),
  ]),
);

/**
 * A mock file as S3 serves a presigned GET: HTTP Range (206 + Content-Range; 416 past the end), and
 * the bucket's CORS rule (GET/HEAD, Range, the length headers exposed) — none at all for
 * `?mockCors=off`, so a browser's cross-origin read fails as a network error. HEAD is refused
 * (403), as S3 refuses it on a URL signed for GET (the method is part of the signature), so a
 * viewer that HEADs a file fails here as it would against S3.
 */
export function serveMockFile(req, res, url) {
  const cors = url.searchParams.get('mockCors') !== 'off';
  if (cors) {
    res.setHeader('access-control-allow-origin', '*');
    res.setHeader('access-control-allow-methods', 'GET, HEAD');
    res.setHeader('access-control-allow-headers', 'range');
    res.setHeader(
      'access-control-expose-headers',
      'content-range, content-length, accept-ranges, etag',
    );
    res.setHeader('access-control-max-age', '3000');
  }
  if (req.method === 'OPTIONS') {
    res.statusCode = cors ? 204 : 403;
    return res.end();
  }
  let name;
  try {
    name = decodeURIComponent(url.pathname.slice('/mock-files/igv/'.length));
  } catch {
    // Not valid percent-encoding: no such key (S3 answers 400 for a malformed request).
    res.statusCode = 400;
    return res.end();
  }
  const body = MOCK_FILES[name];
  if (body && req.method === 'HEAD') {
    res.statusCode = 403;
    return res.end();
  }
  if (!body || req.method !== 'GET') {
    res.statusCode = body ? 405 : 404;
    return res.end();
  }
  res.setHeader('accept-ranges', 'bytes');
  res.setHeader('content-type', 'application/octet-stream');
  res.setHeader('etag', `"${body.length}"`);
  const range = /^bytes=(\d*)-(\d*)$/.exec(req.headers.range ?? '');
  // A range whose last byte comes before its first is invalid, not unsatisfiable: S3 (as RFC 9110
  // §14.2 lets it) ignores it and serves the whole object.
  const invalid = range && range[1] && range[2] && Number(range[2]) < Number(range[1]);
  let start = 0;
  let end = body.length - 1;
  if (range && !invalid && (range[1] || range[2])) {
    if (range[1]) {
      start = Number(range[1]);
      if (range[2]) end = Math.min(end, Number(range[2]));
    } else start = Math.max(0, body.length - Number(range[2]));
    if (start >= body.length || start > end) {
      res.statusCode = 416;
      res.setHeader('content-range', `bytes */${body.length}`);
      return res.end();
    }
    res.statusCode = 206;
    res.setHeader('content-range', `bytes ${start}-${end}/${body.length}`);
  } else res.statusCode = 200;
  res.setHeader('content-length', String(end - start + 1));
  res.end(body.subarray(start, end + 1));
}

/** `PUT /mock-s3/{upload_id}/{part}`: S3 taking one part at its presigned URL, slowly enough for a
 * test to catch an upload in flight. */
function putMockS3Part({ req, res, params, send }) {
  readBody(req, send, (b) => {
    setTimeout(() => {
      const upload = UPLOADS.get(params.upload_id);
      if (!upload) {
        res.statusCode = 404;
        return res.end();
      }
      const n = Number(params.part);
      const etag = `mock-etag-${n}-${b.length}`;
      upload.parts.set(n, { size: b.length, etag });
      res.setHeader('etag', `"${etag}"`);
      res.setHeader('access-control-expose-headers', 'ETag');
      res.statusCode = 200;
      res.end();
    }, PART_DELAY_MS);
  });
}

/** Routes that stand in for something other than the API (S3's presigned URLs): not in the
 * contract. */
export const MOCK_ROUTES = [['PUT', '/mock-s3/{upload_id}/{part}', putMockS3Part]];
