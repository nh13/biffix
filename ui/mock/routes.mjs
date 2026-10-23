// The mock's route table: every route it answers, each as `[method, path template, handler]`, and
// the matching `server.mjs` dispatches by. The API routes' templates are the OpenAPI document's own
// spelling (`/runs/{run_id}/tasks`), so `src/mock-coverage.test.ts` can hold the table to the
// contract. (The alignment view's files, `/mock-files/igv/*`, are served ahead of the table: they
// answer as S3 does, without the API's headers.)
import { AUTH_ROUTES } from './handlers/auth.mjs';
import { DATA_ROUTES } from './handlers/data.mjs';
import { EXPERIMENT_ROUTES } from './handlers/experiments.mjs';
import { ME_ROUTES } from './handlers/me.mjs';
import { OUTPUT_ROUTES } from './handlers/outputs.mjs';
import { QC_ROUTES } from './handlers/qc.mjs';
import { REFERENCE_ROUTES } from './handlers/references.mjs';
import { RUN_ROUTES } from './handlers/runs.mjs';
import { MOCK_ROUTES } from './handlers/s3.mjs';
import { SAMPLE_ROUTES } from './handlers/samples.mjs';
import { SETTINGS_ROUTES } from './handlers/settings.mjs';
import { STATUS_ROUTES } from './handlers/status.mjs';
import { UPLOAD_ROUTES } from './handlers/uploads.mjs';
import { USAGE_ROUTES } from './handlers/usage.mjs';
import { WORKFLOW_ROUTES } from './handlers/workflows.mjs';

/**
 * Every API route the mock answers, as `[method, OpenAPI path template, handler]`. A handler gets
 * `{ req, res, url, query, params, send }`: `query` the request's parameters (a POST `/query`
 * form's, from its body), `params` the path's `{params}` decoded as axum's `Path` decodes them, and
 * `send(status, body)` answering as the backend does (JSON, a plain-text message, or no body).
 */
export const API_ROUTES = [
  ...AUTH_ROUTES,
  ...STATUS_ROUTES,
  ...SETTINGS_ROUTES,
  ...ME_ROUTES,
  ...REFERENCE_ROUTES,
  ...DATA_ROUTES,
  ...UPLOAD_ROUTES,
  ...SAMPLE_ROUTES,
  ...WORKFLOW_ROUTES,
  ...EXPERIMENT_ROUTES,
  ...USAGE_ROUTES,
  ...QC_ROUTES,
  ...OUTPUT_ROUTES,
  ...RUN_ROUTES,
];

/** A route table compiled for matching: a template's `{param}` is one path segment. */
function compileRoutes(routes) {
  return routes.map(([method, path, handler]) => {
    const names = [...path.matchAll(/\{([^}]+)\}/g)].map((m) => m[1]);
    const pattern = path.replace(/[.]/g, '\\.').replace(/\{[^}]+\}/g, '([^/]+)');
    return { method, path, handler, names, re: new RegExp(`^${pattern}$`) };
  });
}
const COMPILED = compileRoutes([...API_ROUTES, ...MOCK_ROUTES]);

/**
 * axum's (matchit's) order between templates that both match a path: at the first segment where
 * they differ, a literal one wins over a `{param}` (`/runs/query` over `/runs/{run_id}`).
 */
function moreLiteral(a, b) {
  const as = a.split('/');
  const bs = b.split('/');
  for (let i = 0; i < Math.min(as.length, bs.length); i++) {
    const ap = as[i].startsWith('{');
    if (ap !== bs[i].startsWith('{')) return ap ? 1 : -1;
  }
  return 0;
}

/**
 * The route for a request, and its decoded path parameters. As axum routes: the path picks one
 * template, the most literal that matches, and then the method picks that template's route.
 * `{ route: null }` when that template has no route for the method (axum's 405, even when a less
 * literal template has one); `null` when no template matches (404); no `params` when a path segment
 * is not valid percent-encoding (axum's `Path` answers 400).
 */
export function routeFor(method, pathname) {
  const matching = COMPILED.filter((r) => r.re.test(pathname));
  if (!matching.length) return null;
  const template = matching.map((r) => r.path).sort(moreLiteral)[0];
  const route = matching.find((r) => r.path === template && r.method === method);
  if (!route) return { route: null };
  const values = route.re.exec(pathname).slice(1);
  try {
    return {
      route,
      params: Object.fromEntries(route.names.map((n, i) => [n, decodeURIComponent(values[i])])),
    };
  } catch {
    return { route };
  }
}
