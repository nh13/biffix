// A hand-maintained mock of the biffix API, for local UI development and the Playwright e2e suite
// (no AWS or Postgres). Its responses follow the generated OpenAPI contract
// (src/lib/generated/openapi.json): `src/mock-contract.test.ts` checks them against it and
// `src/mock-coverage.test.ts` checks that every operation has a route, so an `npm run generate:api`
// that changes the contract fails until the mock follows.
//
// Run: npm run mock (node mock/server.mjs; :8080, or MOCK_PORT; 0 picks a free port), and point the
// UI at it with PUBLIC_BIFFIX_API=http://localhost:8080 (as playwright.config.ts does).
//
// The package, split by resource as `src/lib/api/` is:
// - routes.mjs: the route table (`[method, OpenAPI path, handler]`) and its matching;
// - handlers/<resource>.mjs: each resource's routes, answered as the matching backend handler does
//   (and the logic only that resource needs, such as the Jobs view's paging or the samples CSV);
// - fixtures/<resource>.mjs: the deploy the mock stands for (runs, workflows, data, outputs, …);
// - state.mjs: the caller, and the in-memory stores the writes change;
// - lib/: helpers with no fixtures of their own, the backend logic several handlers share (HTTP
//   bodies, paging order, run-state groups, CSV, FastQC and MultiQC parsing, globs, the sample grid).
//
// The deploy has two projects (`genomics`, the default, and `imported`, with its own bucket and
// data source and uploads off): the data routes take an optional `project` as the backend's do
// (absent: the default; unknown: 400). Beside the API, /mock-files/igv/* serves the tiny BAM, BAI,
// FASTA and FAI the alignment view links to and PUT /mock-s3/* takes upload parts (standing in for
// presigned S3 URLs; see handlers/s3.mjs).
//
// Writes change the mock's in-memory state for the rest of the process, and one mock serves every
// e2e worker: an e2e test that writes works on its own runs, uploads and bundles, or answers the
// route for its own page (see playwright.config.ts).
//
// The signed-in caller is always `alice` (`user:alice`): `mine` lists her runs.
//
// Auth caveat: /auth/config is served, but completing the real OAuth2 PKCE handshake needs a live
// Cognito. For pure data-endpoint work, seed a token first so ensureAuthenticated() passes:
//   sessionStorage.setItem('biffix-access-token', 'mock');
//   sessionStorage.setItem('biffix-access-token-expiry', String(Date.now() + 3600e3));
import { createServer } from 'node:http';
import { keepLiveRunsCurrent } from './handlers/runs.mjs';
import { serveMockFile } from './handlers/s3.mjs';
import { guarded } from './lib/http.mjs';
import { routeFor } from './routes.mjs';
import { ORIGIN, PORT, setOrigin } from './state.mjs';

function handle(req, res, url) {
  keepLiveRunsCurrent();
  res.setHeader('access-control-allow-origin', '*');
  // The app sends a bearer token, which makes every read a preflighted CORS request.
  res.setHeader('access-control-allow-headers', 'authorization, content-type');
  res.setHeader('access-control-allow-methods', 'GET, POST, PUT, DELETE, OPTIONS');
  res.setHeader('content-type', 'application/json');
  if (req.method === 'OPTIONS') {
    res.statusCode = 204;
    return res.end();
  }
  // Answer as the backend does: a JSON body; a string is a plain-text message (its `(StatusCode,
  // String)` errors); no body at all is a bare status (its `StatusCode` errors, and 202/204).
  // A request is answered once: a handler that throws after answering keeps its answer.
  const send = (code, body) => {
    if (res.headersSent) return;
    res.statusCode = code;
    if (body === undefined) {
      res.removeHeader('content-type');
      return res.end();
    }
    if (typeof body === 'string') {
      res.setHeader('content-type', 'text/plain; charset=utf-8');
      return res.end(body);
    }
    res.end(JSON.stringify(body));
  };
  // No route has the path: 404; a route has it, but not for this method: 405; a path parameter
  // that is not valid percent-encoded UTF-8: 400 (as axum answers each).
  const found = routeFor(req.method, url.pathname);
  if (!found) return send(404);
  if (!found.route) return send(405);
  if (!found.params) return send(400, 'Invalid URL: a path segment is not valid percent-encoding');
  // A handler bug answers its own request (500) instead of ending the mock every e2e worker shares.
  guarded(send, 500, '', () =>
    found.route.handler({ req, res, url, query: url.searchParams, params: found.params, send }),
  );
}

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  if (url.pathname.startsWith('/mock-files/igv/')) return serveMockFile(req, res, url);
  handle(req, res, url);
});
server.listen(PORT, () => {
  setOrigin(`http://localhost:${server.address().port}`);
  console.log(`biffix mock API on ${ORIGIN}`);
});
