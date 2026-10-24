// The core of the typed client for the biffix control-plane API: its routes, the one authenticated
// request path and its error mapping. Request and response shapes come directly from the backend's
// utoipa document; `npm run generate:api` refreshes the generated binding. The per-resource modules
// beside this one build on it, and `index.ts` re-exports them.

import { API_BASE } from './base';
import { getAccessToken, reauthenticate, reauthHeld } from './auth';
import type { operations, paths } from '#lib/generated/api.js';

// Every route this client calls, checked against the generated OpenAPI paths: a backend route
// rename fails `svelte-check` here instead of 404ing at runtime. Fill a template with `pathFor`.
export const API_PATHS = {
  runs: '/runs',
  runsQuery: '/runs/query',
  run: '/runs/{run_id}',
  cancel: '/runs/{run_id}/cancel',
  provenance: '/runs/{run_id}/provenance',
  coordinatorLogs: '/runs/{run_id}/coordinator-logs',
  outputsQuery: '/runs/{run_id}/outputs/query',
  tasksQuery: '/runs/{run_id}/tasks/query',
  taskLogsQuery: '/runs/{run_id}/task-logs/query',
  attemptsQuery: '/runs/{run_id}/attempts/query',
  failureSummary: '/runs/{run_id}/failure-summary',
  runSamples: '/runs/{run_id}/samples',
  qcGeneralStats: '/runs/{run_id}/qc/general-stats',
  qcFastqcStatus: '/runs/{run_id}/qc/fastqc-status',
  reports: '/runs/{run_id}/reports',
  output: '/runs/{run_id}/outputs/{output_id}',
  outputDownload: '/runs/{run_id}/outputs/{output_id}/download',
  outputViewUrl: '/runs/{run_id}/outputs/{output_id}/view-url',
  outputPreview: '/runs/{run_id}/outputs/{output_id}/preview',
  outputFastqc: '/runs/{run_id}/outputs/{output_id}/fastqc',
  outputAlignmentView: '/runs/{run_id}/outputs/{output_id}/alignment-view',
  status: '/status',
  statusHistory: '/status/history',
  usage: '/usage',
  workflows: '/workflows',
  workflow: '/workflows/{id}',
  estimate: '/workflows/{id}/estimate',
  engines: '/engines',
  settings: '/settings',
  alerts: '/settings/alerts',
  alertRemove: '/settings/alerts/remove',
  projects: '/projects',
  dataSources: '/data-sources',
  dataList: '/data/list',
  dataFind: '/data/find',
  dataHead: '/data/head',
  dataPreview: '/data/preview',
  dataViewUrl: '/data/view-url',
  dataDownloadUrl: '/data/download-url',
  uploads: '/data/uploads',
  uploadParts: '/data/uploads/{upload_id}/parts',
  uploadComplete: '/data/uploads/{upload_id}/complete',
  uploadStatus: '/data/uploads/{upload_id}/status',
  uploadAbort: '/data/uploads/{upload_id}/abort',
  references: '/references',
  reference: '/references/{id}',
  samplesDetect: '/samples/detect',
  mePrefs: '/me/prefs',
  notifications: '/me/notifications',
  notificationsRead: '/me/notifications/read',
  attentionDismiss: '/me/attention/dismiss',
  workflowListing: '/workflows/listing',
  workflowRelease: '/workflows/{id}/release',
  experiments: '/experiments',
  experiment: '/experiments/{id}',
  experimentSamplesCsv: '/experiments/{id}/samples.csv',
  runExperiment: '/runs/{run_id}/experiment',
  runName: '/runs/{run_id}/name',
} as const satisfies Record<string, keyof paths>;

type PathParams<T extends string> = T extends `${string}{${infer P}}${infer Rest}`
  ? P | PathParams<Rest>
  : never;

/**
 * A route template filled with its path parameters, each encoded as one path segment. The
 * parameter names are checked against the template, so a missing or misspelt one fails to compile.
 */
export function pathFor<T extends keyof paths>(
  template: T,
  params: Record<PathParams<T>, string>,
): string {
  return template.replace(/\{(\w+)\}/g, (_, name: string) =>
    encodeURIComponent((params as Record<string, string>)[name]),
  );
}

/** `path` plus `?query` when the query has any parameter. */
export function withQuery(path: string, query: URLSearchParams): string {
  const q = query.toString();
  return q ? `${path}?${q}` : path;
}

// Operations that actually return 200. The API also has operations answering 201/202 (register a
// workflow, submit a run, engine ingest), and indexing `['responses'][200]` across ALL operations
// stopped type-checking the moment the first of those was added. Constraining the helper says what
// it means — this is the read-path decoder — instead of asserting something untrue about the API.
type OperationWith200 = {
  [K in keyof operations]: operations[K]['responses'] extends { 200: unknown } ? K : never;
}[keyof operations];

type JsonResponse<Operation extends OperationWith200> = operations[Operation]['responses'][200] & {
  content: { 'application/json': unknown };
};
export type JsonBody<Operation extends OperationWith200> =
  JsonResponse<Operation>['content']['application/json'];

// ------------------------------------------------------------------------------------------------
// The one request path: every call to the API (this client's modules' and the upload transport's)
// goes through `authedFetch`, so an expired or rejected session is handled the same way everywhere.

/**
 * The API refused the session: no usable access token, or a 401. A re-authentication (back to the
 * current location) has been started when this is thrown, unless the redirect-loop guard held it
 * back (`redirecting` false); a caller can tell it apart from an ordinary failure and stop retrying.
 */
export class AuthRequiredError extends Error {
  constructor(readonly redirecting = true) {
    super(
      redirecting
        ? 'Your session has expired; signing in again'
        : 'Your session has ended; reload the page to sign in again.',
    );
    this.name = 'AuthRequiredError';
  }
}

/**
 * Start a re-authentication without letting its own failure mask the auth error, and the error to
 * throw: it says whether the sign-in is under way or the loop guard held it back.
 */
function startReauth(): AuthRequiredError {
  const held = reauthHeld();
  reauthenticate().catch(() => {
    // The sign-in config is unreachable: the AuthRequiredError still tells the caller.
  });
  return new AuthRequiredError(!held);
}

/**
 * `fetch` an API path with the caller's access token (and `accept: application/json` unless the
 * init sets it). A missing token or a 401 starts a re-authentication and throws
 * {@link AuthRequiredError}; any other response is returned as is.
 */
export async function authedFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const token = getAccessToken();
  if (!token) throw startReauth();
  const response = await fetch(`${API_BASE}${path}`, {
    ...init,
    headers: {
      accept: 'application/json',
      ...(init.body !== undefined ? { 'content-type': 'application/json' } : {}),
      ...(init.headers as Record<string, string> | undefined),
      authorization: `Bearer ${token}`,
    },
  });
  if (response.status === 401) throw startReauth();
  return response;
}

/** A failed API response: its message (as {@link responseError} words it) and its HTTP status. */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/**
 * The error for a failed response: the server's own message when it sent one (the API answers
 * errors as plain text), else the status and the request. `forbidden` replaces a 403's message
 * with a hint about the missing permission, and `unavailable` a 503's that came without one. It is
 * an {@link ApiError}, so a caller can tell the status apart.
 */
export async function responseError(
  response: Response,
  request: string,
  forbidden?: string,
  unavailable?: string,
): Promise<ApiError> {
  if (response.status === 403 && forbidden) return new ApiError(forbidden, 403);
  const type = response.headers.get('content-type') ?? '';
  // An HTML body is a proxy's error page (CloudFront, a load balancer), not a message for people.
  const detail = type.includes('html') ? '' : (await response.text().catch(() => '')).trim();
  if (!detail && response.status === 503 && unavailable) return new ApiError(unavailable, 503);
  return new ApiError(
    detail || `${response.status} ${response.statusText} (${request})`,
    response.status,
  );
}

interface RequestOptions {
  method?: 'GET' | 'POST' | 'PUT' | 'DELETE';
  /** A JSON request body. */
  body?: unknown;
  accept?: string;
  /** The message for a 403 (default: the server's). */
  forbidden?: string;
  /** The message for a 503 the server gave no reason for (default: the status and request). */
  unavailable?: string;
}

/** An API request whose failure is thrown as {@link responseError}. */
export async function request(path: string, opts: RequestOptions = {}): Promise<Response> {
  const method = opts.method ?? 'GET';
  const response = await authedFetch(path, {
    method,
    ...(opts.body !== undefined ? { body: JSON.stringify(opts.body) } : {}),
    ...(opts.accept ? { headers: { accept: opts.accept } } : {}),
  });
  if (!response.ok)
    throw await responseError(response, `${method} ${path}`, opts.forbidden, opts.unavailable);
  return response;
}

/** An API request answered with JSON. */
export async function requestJson<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  return (await (await request(path, opts)).json()) as T;
}

export const get = <T>(path: string) => requestJson<T>(path);

/** {@link get}, with a 404 read as "none" (`null`). */
export async function getOrNull<T>(path: string): Promise<T | null> {
  try {
    return await get<T>(path);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}

/** A 403's message for the developer-only calls. */
export const DEVELOPER_ONLY = 'Requires developer access';
