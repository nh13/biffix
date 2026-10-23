// The e2e mock's route table (`mock/routes.mjs`) against the API contract: every operation the
// generated OpenAPI document defines (method + path) has a mock route, or is named below as one the
// mock leaves out on purpose; and the mock has no API route the contract does not define. So a
// backend route added, renamed or removed fails here until the mock follows it
// (`mock-contract.test.ts` then checks the answers' shapes).

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const doc = JSON.parse(
  readFileSync(fileURLToPath(new URL('./lib/generated/openapi.json', import.meta.url)), 'utf8'),
) as { paths: Record<string, Record<string, unknown>> };

/** The operations the mock does not serve, each with why the UI never calls it. */
const NOT_MOCKED: Record<string, string> = {
  // Probes for the load balancer and the deploy's health check.
  'GET /healthz': 'a liveness probe',
  'GET /readyz': 'a readiness probe',
  // The engine plugins' event stream (a per-run capability token), not a browser's.
  'POST /events': "the coordinator plugins' event ingest",
  // Registration is the CLI's (`biffix workflows register`, …): the UI has no form.
  'POST /workflows': 'registering a workflow version (CLI)',
  'POST /workflows/source-upload': "a registration's source upload (CLI)",
  'POST /workflows/{id}/reregister': 're-registering a version (CLI)',
  // A failed build's recovery (docs/deploying.md), sent as a raw API call: no client calls these.
  'POST /workflows/{id}/runtime/rebuild': "rebuilding a version's engine runtime (raw API call)",
  'POST /workflows/{id}/images/{name}/rebuild':
    "rebuilding one of a version's images (raw API call)",
};

const METHODS = new Set(['get', 'put', 'post', 'delete', 'patch', 'head', 'options']);
const CONTRACT = Object.entries(doc.paths).flatMap(([path, ops]) =>
  Object.keys(ops)
    .filter((m) => METHODS.has(m))
    .map((m) => `${m.toUpperCase()} ${path}`),
);

// A computed specifier: the mock is plain JavaScript, loaded at run time rather than type-checked
// through this file.
const routesModule = new URL('../mock/routes.mjs', import.meta.url).href;
const { API_ROUTES } = (await import(/* @vite-ignore */ routesModule)) as {
  API_ROUTES: [string, string, unknown][];
};
const ROUTED = API_ROUTES.map(([method, path]) => `${method} ${path}`);

describe('the mock covers the API contract', () => {
  it('routes every operation, or names it as left out', () => {
    const missing = CONTRACT.filter((op) => !ROUTED.includes(op) && !(op in NOT_MOCKED));
    expect(missing, 'operations with no mock route (add one, or to NOT_MOCKED)').toEqual([]);
  });

  it('routes nothing the contract does not define', () => {
    const extra = ROUTED.filter((op) => !CONTRACT.includes(op));
    expect(extra, 'mock routes the contract has no operation for').toEqual([]);
  });

  it('routes each operation once', () => {
    expect(ROUTED.filter((op, i) => ROUTED.indexOf(op) !== i)).toEqual([]);
  });

  it('leaves out only operations the contract has, and serves none of them', () => {
    for (const op of Object.keys(NOT_MOCKED)) {
      expect(CONTRACT, `${op} is no longer in the contract`).toContain(op);
      expect(ROUTED, `${op} is routed: drop it from NOT_MOCKED`).not.toContain(op);
    }
  });
});
