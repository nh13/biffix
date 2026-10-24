// The e2e mock (`mock/server.mjs`) against the API contract: every answer it gives is checked
// against the generated OpenAPI document (the one `npm run generate:api` writes from the backend),
// strictly — a field the contract does not define fails, so a leaked or renamed field is caught —
// and against the invariants the backend keeps (counters that agree with the task list, the
// listings' orders, plain-text errors). A contract change fails here until the mock follows it.
//
// The mock is spawned on a free port (it keeps state, so each run of this file starts clean), and
// the requests run in order: the launches and cancels below see each other's effects.

import { spawn, type ChildProcess } from 'node:child_process';
import { readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { RunDetail } from '#lib/api/index.js';

type Json = null | boolean | number | string | Json[] | { [k: string]: Json };
interface Schema {
  $ref?: string;
  // OpenAPI 3.1: a nullable value is `type: [T, 'null']`, or `oneOf` with a `{type: 'null'}` arm.
  type?: string | string[];
  enum?: Json[];
  properties?: Record<string, Schema>;
  required?: string[];
  additionalProperties?: boolean | Schema;
  items?: Schema;
  allOf?: Schema[];
  oneOf?: Schema[];
}
interface Operation {
  responses: Record<string, { content?: Record<string, { schema: Schema }> }>;
}

const doc = JSON.parse(
  readFileSync(fileURLToPath(new URL('./lib/generated/openapi.json', import.meta.url)), 'utf8'),
) as {
  paths: Record<string, Record<string, Operation>>;
  components: { schemas: Record<string, Schema> };
};

const resolve = (s: Schema): Schema => {
  while (s.$ref) s = doc.components.schemas[s.$ref.replace('#/components/schemas/', '')];
  return s;
};
/** Every property an (allOf-composed) object schema defines. */
const propsOf = (s: Schema): Record<string, Schema> =>
  Object.assign({}, ...(resolve(s).allOf ?? []).map(propsOf), resolve(s).properties ?? {});

/** A schema's declared types (`type` is one name or, in OpenAPI 3.1, a list). */
const typesOf = (s: Schema): string[] =>
  s.type === undefined ? [] : Array.isArray(s.type) ? s.type : [s.type];
/** A schema's one non-null type, if it declares one. */
const typeOf = (s: Schema): string | undefined => typesOf(s).find((t) => t !== 'null');

/** Where `v` breaks schema `s` (empty when it conforms). Objects are closed: an undefined field is
 * an error unless the schema says `additionalProperties`. */
function violations(s: Schema, v: Json | undefined, at = '$'): string[] {
  s = resolve(s);
  if (typesOf(s).includes('null')) {
    if (v === null) return [];
    if (typeOf(s) === undefined) return [`${at}: ${JSON.stringify(v)} where null is required`];
  }
  // A null is admitted by a `{type: 'null'}` arm of a `oneOf`, checked below.
  if (v === null && !s.oneOf) return [`${at}: null where ${typeOf(s) ?? 'a value'} is required`];
  if (v === undefined) return [`${at}: missing`];
  // One of several shapes (a tagged union): it conforms when one variant admits it.
  if (s.oneOf) {
    const tried = s.oneOf.map((variant) => violations(variant, v, at));
    return tried.some((t) => t.length === 0)
      ? []
      : [`${at}: matches none of its variants (${tried.map((t) => t[0]).join('; ')})`];
  }
  const out: string[] = [];
  if (s.allOf) {
    const parts = s.allOf.map(resolve);
    for (const p of parts) {
      // Required fields and each field's type from every part; openness from the union below.
      out.push(...violations({ ...p, additionalProperties: true }, v, at));
    }
    // A wrapped scalar (`allOf: [$ref to an enum]`) or union is its part, and done.
    if (!parts.some((p) => typeOf(p) === 'object' || p.properties || p.allOf)) return out;
  }
  if (s.enum && !s.enum.includes(v)) out.push(`${at}: ${JSON.stringify(v)} not in ${s.enum}`);
  const kind = Array.isArray(v) ? 'array' : typeof v;
  const type = typeOf(s);
  switch (type) {
    case 'integer':
      if (!Number.isInteger(v)) out.push(`${at}: ${JSON.stringify(v)} is not an integer`);
      return out;
    case 'number':
    case 'string':
    case 'boolean':
      if (kind !== type) out.push(`${at}: ${kind} where ${type} is required`);
      return out;
    case 'array':
      if (!Array.isArray(v)) return [...out, `${at}: ${kind} where array is required`];
      if (s.items) v.forEach((x, i) => out.push(...violations(s.items!, x, `${at}[${i}]`)));
      return out;
  }
  if (type === 'object' || s.properties || s.allOf) {
    if (kind !== 'object') return [...out, `${at}: ${kind} where object is required`];
    const obj = v as Record<string, Json>;
    for (const r of s.required ?? []) if (!(r in obj)) out.push(`${at}.${r}: missing`);
    const props = propsOf(s);
    for (const [k, x] of Object.entries(obj)) {
      if (props[k]) {
        if (!s.allOf || s.properties?.[k]) out.push(...violations(props[k], x, `${at}.${k}`));
      } else if (typeof s.additionalProperties === 'object') {
        out.push(...violations(s.additionalProperties, x, `${at}.${k}`));
      } else if (s.additionalProperties !== true && Object.keys(props).length > 0) {
        out.push(`${at}.${k}: not in the contract`);
      }
    }
  }
  return out;
}

// The contract's operations, as path patterns (`{id}` is one decoded segment).
const OPS = Object.entries(doc.paths).flatMap(([path, ops]) =>
  Object.entries(ops).map(([method, op]) => ({
    method: method.toUpperCase(),
    path,
    re: new RegExp(`^${path.replace(/\{[^}]+\}/g, '[^/]+')}$`),
    op,
  })),
);
const opFor = (method: string, pathname: string) =>
  OPS.filter((o) => o.method === method && o.re.test(pathname))
    // The most literal template wins (`/runs/query` over `/runs/{run_id}`).
    .sort((a, b) => a.path.split('{').length - b.path.split('{').length)[0];

/** Spawn a fresh mock on a free port, with `env` added to its environment; resolves once it
 * listens, to the process and its origin. */
async function startMock(env: Record<string, string> = {}): Promise<[ChildProcess, string]> {
  const script = fileURLToPath(new URL('../mock/server.mjs', import.meta.url));
  const proc = spawn(process.execPath, [script], {
    env: { ...process.env, ...env, MOCK_PORT: '0' },
  });
  const at = await new Promise<string>((ok, fail) => {
    let out = '';
    proc.stdout!.on('data', (c: Buffer) => {
      out += c;
      const m = /on (http:\/\/\S+)/.exec(out);
      if (m) ok(m[1]);
    });
    proc.on('exit', (code) => fail(new Error(`the mock exited (${code})`)));
  });
  return [proc, at];
}

let mock: ChildProcess;
let origin = '';
beforeAll(async () => {
  [mock, origin] = await startMock();
});
afterAll(() => {
  mock?.kill();
});

/** Routed operations no success answer is checked for here, each with why. */
const NOT_CHECKED: Record<string, string> = {};

/**
 * Operations whose success body the contract leaves an untyped object (`body = Object` in the
 * backend's annotation), each with why: the strict check cannot see a leaked or renamed field in
 * them, so a new one fails until it is typed in the backend or listed here.
 */
const OPAQUE: Record<string, string> = {
  'GET /runs/{run_id}/samples': 'SampleGrid is `body = Object` in api/runs.rs',
  'POST /samples/detect': 'DetectedSamples is `body = Object` in api/samples.rs',
  'GET /runs/{run_id}/provenance': 'a Workflow Run RO-Crate (JSON-LD), not a biffix type',
};

/** Whether a schema says nothing of an object's fields (an untyped `Object`). */
const isOpaque = (s: Schema): boolean => {
  const r = resolve(s);
  return (
    typeOf(r) === 'object' &&
    Object.keys(r.properties ?? {}).length === 0 &&
    !r.allOf &&
    r.additionalProperties === undefined
  );
};

/** The operations (`METHOD /path/{template}`) whose 2xx answer a `call` checked. */
const checked = new Set<string>();

interface Answer {
  status: number;
  body: Json | undefined;
  text: string;
  type: string;
}

/**
 * One request to the mock, checked: a 2xx must be a status the contract documents for the route,
 * with a body its schema admits; any other answer must be plain text or empty, as the backend's
 * errors are.
 */
async function call(method: string, path: string, body?: unknown): Promise<Answer> {
  const res = await fetch(`${origin}${path}`, {
    method,
    ...(body === undefined
      ? {}
      : { body: JSON.stringify(body), headers: { 'content-type': 'application/json' } }),
  });
  const text = await res.text();
  const type = res.headers.get('content-type') ?? '';
  const pathname = decodeURIComponent(new URL(path, origin).pathname);
  if (res.status >= 300) {
    expect(type.startsWith('application/json'), `${method} ${path} → ${res.status}: ${text}`).toBe(
      false,
    );
    return { status: res.status, body: undefined, text, type };
  }
  const op = opFor(method, pathname);
  expect(op, `${method} ${pathname} is not in the contract`).toBeDefined();
  const documented = op!.op.responses[String(res.status)];
  expect(documented, `${method} ${op!.path} does not document ${res.status}`).toBeDefined();
  checked.add(`${method} ${op!.path}`);
  const content = documented.content ?? {};
  // A text body (a CSV download): the type the contract names, and the text as is.
  const textType = Object.keys(content).find((t) => t.startsWith('text/'));
  if (textType) {
    expect(type.startsWith(textType), `${method} ${path} → ${type}, not ${textType}`).toBe(true);
    return { status: res.status, body: undefined, text, type };
  }
  const schema = Object.values(content)[0]?.schema;
  if (!schema) {
    expect(text, `${method} ${path} → ${res.status} has no body in the contract`).toBe('');
    return { status: res.status, body: undefined, text, type };
  }
  const parsed = JSON.parse(text) as Json;
  expect(violations(schema, parsed), `${method} ${path}`).toEqual([]);
  const name = `${method} ${op!.path}`;
  expect(
    isOpaque(schema),
    `${name}: an untyped object in the contract (type it, or list it in OPAQUE)`,
  ).toBe(name in OPAQUE);
  return { status: res.status, body: parsed, text, type };
}

const obj = (a: Answer) => a.body as Record<string, Json>;
const list = <T = Record<string, Json>>(a: Answer, key: string) => obj(a)[key] as unknown as T[];
const q = (id: string) => encodeURIComponent(id);

type RunCost = NonNullable<RunDetail['cost']>;
const near = (a: number, b: number) => Math.abs(a - b) < 1e-9;
/**
 * `cost` is one the backend's `store::run_cost` can produce: present only with a priced or pending
 * job; the estimate is compute + root volume, every share of it no more than it, and its breakdown
 * by instance type adds up to it; nothing priced means nothing estimated.
 */
function expectPossibleCost(cost: RunCost, id: string) {
  const { estimated_usd: usd, by_instance_type: types } = cost;
  expect(cost.priced_jobs > 0 || cost.pending_jobs > 0, id).toBe(true);
  expect(near(usd, cost.compute_usd + cost.ebs_usd), `${id}: estimate is compute + ebs`).toBe(true);
  expect(cost.coordinator_usd <= usd + 1e-9 && cost.winning_usd <= usd + 1e-9, id).toBe(true);
  expect(
    types.reduce((n, t) => n + t.jobs, 0),
    `${id}: breakdown jobs`,
  ).toBe(cost.priced_jobs);
  expect(
    near(
      types.reduce((n, t) => n + t.usd, 0),
      usd,
    ),
    `${id}: breakdown usd`,
  ).toBe(true);
  if (cost.priced_jobs === 0)
    expect([usd, cost.coordinator_usd, cost.winning_usd, cost.price_sources.length], id).toEqual([
      0, 0, 0, 0,
    ]);
}

describe('the mock API', () => {
  it('serves the shell reads as the contract says', async () => {
    const auth = await call('GET', '/auth/config');
    expect(obj(auth).redirect_uri).toBe('http://localhost:5173/auth/callback');
    await call('GET', '/settings');
    await call('GET', '/engines');
    await call('GET', '/data-sources');
    const status = obj(await call('GET', '/status'));
    // The overall status is the worst component's.
    const components = status.components as { status: string }[];
    const worst = ['ok', 'degraded', 'down'][
      Math.max(...components.map((c) => ['ok', 'degraded', 'down'].indexOf(c.status)))
    ];
    expect(status.status).toBe(worst);
    for (const usage of ['month', 'workflow', 'submitter', 'instance_type', 'project'])
      await call('GET', `/usage?group_by=${usage}`);
    expect((await call('GET', '/usage?group_by=nope')).status).toBe(400);
    // As usage.rs reads them: a blank grouping is `month`; the dates are real YYYY-MM-DD days.
    expect(obj(await call('GET', '/usage?group_by=')).group_by).toBe('month');
    const span = obj(await call('GET', '/usage?from=2026-01-01&to=2026-02-01'));
    expect([span.from, span.to]).toEqual(['2026-01-01', '2026-02-01']);
    for (const [args, message] of [
      ['from=2026-02-30&to=2026-03-01', '`2026-02-30` is not a date (YYYY-MM-DD)'],
      ['from=2026-9-1&to=2026-10-01', '`2026-9-1` is not a date (YYYY-MM-DD)'],
      ['from=2026-03-01&to=2026-02-01', '`to` must be after `from`'],
    ]) {
      const bad = await call('GET', `/usage?${args}`);
      expect([bad.status, bad.text]).toEqual([400, message]);
    }
    // An inherited property is no grouping, and no window.
    expect((await call('GET', '/usage?group_by=constructor')).status).toBe(400);
    expect((await call('GET', '/status/history?window=toString')).status).toBe(400);
  });

  it('answers a malformed request with a 4xx, and keeps serving', async () => {
    // One mock serves every e2e worker: a body or path a handler cannot take must answer that
    // request, as axum would, never end the process.
    const refused = async (method: string, path: string, body: string) => {
      const res = await fetch(`${origin}${path}`, {
        method,
        body,
        headers: { 'content-type': 'application/json' },
      });
      return [res.status, await res.text()] as const;
    };
    const [nullStatus, nullText] = await refused('POST', '/data/head', 'null');
    expect([nullStatus, nullText]).toEqual([
      422,
      'Failed to deserialize the JSON body into the target type: invalid type: null, expected a JSON object',
    ]);
    expect((await refused('POST', '/data/head', '[]'))[0]).toBe(422);
    expect((await refused('POST', '/data/head', '{"uris":null}'))[0]).toBe(422);
    expect((await refused('POST', '/data/uploads', 'null'))[0]).toBe(422);
    expect((await refused('PUT', '/workflows/listing', '5'))[0]).toBe(422);
    const images = JSON.stringify({ workflow_version_id: 'aabbccddeeff:v1', images: 'x' });
    expect((await refused('POST', '/runs', images))[0]).toBe(422);
    // axum's `Json` wants the body labelled JSON (415), and a body at all (400).
    const unlabelled = await fetch(`${origin}/me/notifications/read`, { method: 'POST' });
    expect([unlabelled.status, await unlabelled.text()]).toEqual([
      415,
      'Expected request with `Content-Type: application/json`',
    ]);
    expect((await refused('POST', '/me/notifications/read', ''))[0]).toBe(400);
    // A path segment that is not valid percent-encoding: axum's `Path` answers 400.
    expect((await fetch(`${origin}/runs/%E0%A4%A`)).status).toBe(400);
    expect((await fetch(`${origin}/mock-files/igv/%E0%A4%A`)).status).toBe(400);
    expect((await call('GET', '/status')).status).toBe(200);
  });

  it('routes by the most literal path first, then by method, as axum does', async () => {
    // `/runs/query` is POST only: a GET is a 405, not `GET /runs/{run_id}` with the id `query`.
    expect((await call('GET', '/runs/query')).status).toBe(405);
    expect((await call('GET', '/runs/run_01HGW1failed/outputs/query')).status).toBe(405);
    expect((await call('GET', '/runs/run_01HGW1failed')).status).toBe(200);
    expect((await call('GET', '/no/such/path')).status).toBe(404);
  });

  it('sizes the status history by its window, and refuses an unknown one', async () => {
    const spans: Record<string, [number, number]> = {
      '1h': [3_600, 300],
      '1d': [86_400, 960],
      '1w': [604_800, 6_720],
      '90d': [7_776_000, 86_400],
    };
    for (const [window, [span, bucket]] of Object.entries(spans)) {
      const h = obj(await call('GET', `/status/history?window=${window}`));
      expect([h.window_secs, h.bucket_secs]).toEqual([span, bucket]);
      expect(h.expected_per_bucket).toBe(Math.max(1, Math.floor(bucket / 300)));
    }
    expect(obj(await call('GET', '/status/history')).window_secs).toBe(7_776_000);
    const bad = await call('GET', '/status/history?window=2d');
    expect([bad.status, bad.text]).toEqual([400, 'unknown window `2d` (1h, 1d, 1w or 90d)']);
  });

  it('lists runs as RunSummary rows in the keyset order, paged at most 200', async () => {
    const page = await call('POST', '/runs/query', { limit: 1000 });
    expect(obj(page).limit).toBe(200);
    expect(obj(await call('POST', '/runs/query', { limit: 0 })).limit).toBe(1);
    // A `limit` that is not an integer is the typed field's refusal, not the default: axum's
    // `Query` answers 400, its `Json` 422.
    for (const path of [
      '/runs?limit=',
      '/runs?limit=2.5',
      '/me/notifications?limit=abc',
      // So are the other typed parameters: a boolean, an offset, a sample count.
      '/runs?mine=yes',
      '/runs?attention=1',
      '/experiments?archived=yes',
      '/experiments?mine=',
      `/workflows/${q('aabbccddeeff:v1')}/estimate?samples=-1`,
      `/workflows/${q('aabbccddeeff:v1')}/estimate?samples=two`,
    ])
      expect((await call('GET', path)).status, path).toBe(400);
    for (const [path, body] of [
      ['/runs/query', { limit: 2.5 }],
      ['/runs/run_01HGW3align/tasks/query', { limit: 'x' }],
      ['/runs/run_01HGW3align/tasks/query', { offset: 'abc' }],
      ['/runs/run_01HGW3align/outputs/query', { limit: 1.5 }],
      ['/data/find', { source: 'refs', q: 'x', limit: 'x' }],
    ] as const)
      expect((await call('POST', path, body)).status, path).toBe(422);
    const mine = list<{ submitter: string }>(
      await call('POST', '/runs/query', { mine: true }),
      'runs',
    );
    expect(mine.length).toBeGreaterThan(0);
    expect(mine.every((r) => r.submitter === 'user:alice')).toBe(true);
    const runs = list(page, 'runs');
    expect(runs.length).toBeGreaterThan(5);
    for (const f of ['active', 'submitted', 'completed', 'succeeded', 'failed'])
      await call('POST', '/runs/query', { filter: f });
    // `all` is every run, as an absent filter is; any other spelling, a blank one too, is a 400.
    expect(obj(await call('POST', '/runs/query', { filter: 'all' })).total).toBe(obj(page).total);
    const accepted = 'all, active, submitted, completed, succeeded or failed';
    for (const [path, body, f] of [
      ['/runs?filter=bogus', undefined, 'bogus'],
      ['/runs?filter=', undefined, ''],
      ['/runs/query', { filter: '' }, ''],
    ] as const) {
      const bad = await call(body ? 'POST' : 'GET', path, body);
      expect([bad.status, bad.text]).toEqual([400, `unknown filter \`${f}\` (${accepted})`]);
    }

    // Newest first, ties by run id descending: walk the paging version's 120 runs in pages of 50
    // (their page edges fall inside submitted_at ties) and see every run once, in order.
    const seen: Record<string, Json>[] = [];
    let after: string | undefined;
    do {
      const p = obj(
        await call('POST', '/runs/query', { workflow: 'paging-demo:v1', limit: 50, after }),
      );
      seen.push(...(p.runs as Record<string, Json>[]));
      after = (p.runs as Json[]).length === 50 ? (p.last_cursor as string) : undefined;
    } while (after);
    expect(seen.map((r) => r.run_name)).toEqual(
      Array.from({ length: 120 }, (_, i) => `paging run ${i + 1}`),
    );
    const ties = seen.filter((r, i) => i > 0 && r.submitted_at === seen[i - 1].submitted_at);
    expect(ties.length).toBeGreaterThan(0);
    // A cursor the mock did not issue is no cursor: the first page, in the normal order.
    const firstPage = obj(
      await call('POST', '/runs/query', { workflow: 'paging-demo:v1', limit: 50 }),
    );
    for (const cursor of [{ after: 'garbage' }, { before: 'garbage' }, { after: 'x|y' }]) {
      const p = obj(
        await call('POST', '/runs/query', { workflow: 'paging-demo:v1', limit: 50, ...cursor }),
      );
      expect(p.runs, JSON.stringify(cursor)).toEqual(firstPage.runs);
    }
    // No run ends before it was submitted, as the projection can never record.
    const backwards = seen.filter((r) => (r.completed_at as number) < (r.submitted_at as number));
    expect(backwards.map((r) => r.run_id)).toEqual([]);
  });

  it('names runs by id, at most a page of them, as runs.rs does', async () => {
    const named = obj(
      await call('POST', '/runs/query', {
        ids: ['run_01HGWDdone', 'run_nope', ' ', 'run_01HGW3align'],
      }),
    );
    expect((named.runs as Record<string, Json>[]).map((r) => r.run_id).sort()).toEqual([
      'run_01HGW3align',
      'run_01HGWDdone',
    ]);
    expect(named.total).toBe(2);
    const all = obj(await call('POST', '/runs/query', {}));
    expect(obj(await call('POST', '/runs/query', { ids: [] })).total).toBe(all.total);
    // Blank ids only name no run, never every run.
    const blank = obj(await call('POST', '/runs/query', { ids: [' ', ''] }));
    expect(blank.total).toBe(0);
    expect(blank.runs).toEqual([]);
    // A page's worth is accepted; more is refused.
    const page = Array.from({ length: 200 }, (_, i) => `r${i}`);
    expect(obj(await call('POST', '/runs/query', { ids: page })).total).toBe(0);
    const many = Array.from({ length: 201 }, (_, i) => `r${i}`);
    expect((await call('POST', '/runs/query', { ids: many })).status).toBe(400);
    // A query string carries no list.
    expect((await call('GET', '/runs?ids=run_01HGWDdone')).status).toBe(400);
  });

  it('renames a run and clears its name, as runs.rs rename_run answers', async () => {
    // A run started without a name (from the CLI); its name is put back off at the end.
    const id = 'run_01HGW4resume';
    expect(obj(await call('GET', `/runs/${id}`)).run_name).toBeUndefined();
    const named = obj(await call('POST', `/runs/${id}/name`, { run_name: '  Liver batch 2 ' }));
    expect(named.run_name).toBe('Liver batch 2');
    expect(obj(await call('GET', `/runs/${id}`)).run_name).toBe('Liver batch 2');
    const cleared = obj(await call('POST', `/runs/${id}/name`, { run_name: null }));
    expect(cleared.run_name).toBeUndefined();
    expect(obj(await call('GET', `/runs/${id}`)).run_name).toBeUndefined();
    // No `run_name` is refused (never read as a clear), in the backend's words.
    const missing = await call('POST', `/runs/${id}/name`, { name: 'x' });
    expect([missing.status, missing.text]).toEqual([
      400,
      'the body must be JSON naming `run_name`: a name, or null to clear it',
    ]);
    expect((await call('POST', '/runs/nope/name', { run_name: 'x' })).status).toBe(404);
  });

  it('searches the pipeline title the Lab shows', async () => {
    const hits = list(await call('POST', '/runs/query', { q: 'bulk rna', limit: 200 }), 'runs');
    expect(hits.length).toBeGreaterThan(0);
    for (const r of hits) expect(String(r.workflow_title ?? '')).toMatch(/bulk rna/i);
  });

  it("serves every run's detail and reads, agreeing with its task list and outputs", async () => {
    const runs = list(await call('POST', '/runs/query', { limit: 200 }), 'runs');
    for (const summary of runs) {
      const id = summary.run_id as string;
      const detail = obj(await call('GET', `/runs/${q(id)}`));
      const tasks = obj(await call('POST', `/runs/${q(id)}/tasks/query`, { limit: 1000 }));
      // The list's cost is the run page's: its estimate once a job is priced, else none; and
      // pending while the page counts a job not priced yet.
      const cost = detail.cost as RunCost | undefined;
      if (cost) expectPossibleCost(cost, id);
      const priced = cost && cost.priced_jobs > 0 ? cost.estimated_usd : undefined;
      expect([summary.cost_usd, detail.cost_usd], id).toEqual([priced, priced]);
      const pending = (cost?.pending_jobs ?? 0) > 0;
      expect([summary.cost_pending, detail.cost_pending], id).toEqual([pending, pending]);
      // The counters are the task list's.
      const all = detail.tasks as { state: string; cached?: boolean }[];
      const count = (st: string) => all.filter((t) => t.state === st).length;
      expect(tasks.total, id).toBe(all.length);
      expect(
        [
          detail.tasks_observed,
          detail.tasks_succeeded,
          detail.tasks_running,
          detail.tasks_queued,
          detail.tasks_failed,
          detail.tasks_cancelled,
          detail.tasks_cached,
        ],
        id,
      ).toEqual([
        all.length,
        count('succeeded'),
        count('running'),
        count('queued'),
        count('failed'),
        count('cancelled'),
        all.filter((t) => t.cached).length,
      ]);
      // As are the DAG's node counts.
      const dag = detail.dag as { nodes: Record<string, number>[] };
      const counted = dag.nodes.reduce(
        (n, x) => n + x.pending + x.running + x.succeeded + x.failed,
        0,
      );
      expect(counted, id).toBe(all.length);
      // A default-prefix run's prefix is its own.
      if (!detail.resumed_from)
        expect(detail.output_prefix as string, id).toMatch(new RegExp(`/biffix/runs/${id}/$`));
      // And the output count is the list's (intermediates aside, and the files a resumed run
      // reused, which the list adds to what the run registered).
      const outs = obj(await call('POST', `/runs/${q(id)}/outputs/query`, { limit: 2000 }));
      const reused = (outs.outputs as { reused_from?: string }[]).filter(
        (o) => o.reused_from,
      ).length;
      expect((outs.total as number) - reused, id).toBe(summary.outputs_total);
      expect(outs.output_prefix, id).toBe(detail.output_prefix);
      await call('GET', `/runs/${q(id)}/failure-summary`);
      await call('GET', `/runs/${q(id)}/reports`);
      await call('GET', `/runs/${q(id)}/provenance`);
      await call('GET', `/runs/${q(id)}/coordinator-logs`);
      // A termination category is spelled as `TerminationCategory` serializes (`oom`, not `OOM`).
      for (const t of tasks.tasks as { task_key: string; termination_category?: string }[])
        if (t.termination_category != null)
          expect(['spot', 'oom', 'host_terminated', 'app_error', 'missing_output'], id).toContain(
            t.termination_category,
          );
      // Ordered by key, bytewise (JS's default sort), as the backend's `str` order.
      const keys = (tasks.tasks as { task_key: string }[]).map((t) => t.task_key);
      expect(keys, id).toEqual([...keys].sort());
      const first = (tasks.tasks as { task_key: string }[])[0];
      if (first) {
        await call('POST', `/runs/${q(id)}/task-logs/query`, { task: first.task_key });
        await call('POST', `/runs/${q(id)}/attempts/query`, { task: first.task_key });
      }
    }
    expect((await call('GET', '/runs?filter=active&limit=5')).status).toBe(200);
    const nope = await call('GET', '/runs/nope');
    expect([nope.status, nope.text]).toEqual([404, '']);
    const noTasks = await call('POST', '/runs/nope/tasks/query', {});
    expect([noTasks.status, noTasks.text]).toEqual([404, '']);
  });

  it('reads a run’s logs per run, task and stream, as logs.rs does', async () => {
    const LIVE = '/runs/run_01HGW3align';
    const logs = (run: string, body: object) => call('POST', `${run}/task-logs/query`, body);
    const status = async (run: string, body: object) => (await logs(run, body)).status;
    expect(await status('/runs/nope', { task: 'x' })).toBe(404);
    expect((await call('GET', '/runs/nope/coordinator-logs')).status).toBe(404);
    expect(await status(LIVE, {})).toBe(422);
    expect(await status(LIVE, { task: 'nope' })).toBe(404);
    const align = 'align:sample=HG00096';
    expect(await status(LIVE, { task: align, job: 'job-99' })).toBe(404);
    expect(await status(LIVE, { task: align, direction: 'sideways' })).toBe(400);
    const log = obj(await logs(LIVE, { task: align }));
    expect([log.log_stream, (log.events as Json[]).length]).toEqual([
      'mock/align:sample=HG00096',
      3,
    ]);
    // A queued task's job has no stream yet: an empty page, not a 404.
    const empty = {
      log_stream: null,
      events: [],
      next_forward_token: null,
      next_backward_token: null,
    };
    expect(obj(await logs(LIVE, { task: 'joint_call' }))).toEqual(empty);
    // Nor has a just-launched run's coordinator.
    const launched = obj(
      await call('POST', '/runs', { workflow_version_id: 'aabbccddeeff:v1', params: {} }),
    );
    expect(obj(await call('GET', `/runs/${launched.run_id}/coordinator-logs`))).toEqual(empty);
    expect((obj(await call('GET', `${LIVE}/coordinator-logs`)).events as Json[]).length).toBe(3);
  });

  it('sorts, pages and refuses task queries as page_tasks does', async () => {
    const page = (body: object) =>
      call('POST', `/runs/run_01HGW3align/tasks/query`, body).then((a) =>
        list<{ task_key: string }>(a, 'tasks').map((t) => t.task_key),
      );
    expect(await page({})).toEqual([...(await page({}))].sort());
    // Longest first, unstarted last (joint_call is queued).
    expect(await page({ sort: 'duration' })).toEqual([
      'align:sample=HG00096',
      'align:sample=HG00097',
      'markdup:sample=HG00096',
      'fastqc:sample=HG00096',
      'joint_call',
    ]);
    // Most recently started first.
    expect(await page({ sort: 'started' })).toEqual([
      'markdup:sample=HG00096',
      'align:sample=HG00096',
      'align:sample=HG00097',
      'fastqc:sample=HG00096',
      'joint_call',
    ]);
    expect((await page({ sort: 'started', order: 'desc' }))[0]).toBe('joint_call');
    for (const [name, value, words] of [
      ['sort', 'size', 'key, state, duration or started'],
      ['state', 'queued', 'all, active, succeeded or failed'],
      ['order', 'up', 'asc or desc'],
    ]) {
      const a = await call('POST', '/runs/run_01HGW3align/tasks/query', { [name]: value });
      expect([a.status, a.text]).toEqual([400, `unknown ${name} \`${value}\` (${words})`]);
    }
    const big = obj(await call('POST', '/runs/run_01HGW9big/tasks/query', { limit: 5000 }));
    expect(big.limit).toBe(1000);
  });

  it("pages a run's outputs by S3 URI", async () => {
    const first = obj(await call('POST', '/runs/run_01HGW9big/outputs/query', { limit: 2000 }));
    const rows = first.outputs as { s3_uri: string }[];
    expect(rows).toHaveLength(2000);
    expect(rows.map((r) => r.s3_uri)).toEqual(rows.map((r) => r.s3_uri).sort());
    expect(first.next_cursor).toBe(rows.at(-1)!.s3_uri);
    const second = obj(
      await call('POST', '/runs/run_01HGW9big/outputs/query', {
        limit: 2000,
        after: first.next_cursor,
      }),
    );
    expect(second.outputs as Json[]).toHaveLength(50);
    expect(second.next_cursor).toBeUndefined();
    const under = obj(
      await call('POST', '/runs/run_01HGW2wgs30x/outputs/query', { prefix: 'qc/' }),
    );
    expect((under.outputs as { path: string }[]).map((o) => o.path)).toEqual([
      'qc/NA12878.umi_grouping.metrics',
      'qc/coverage.svg',
      'qc/multiqc_report.html',
    ]);
    expect((await call('POST', '/runs/nope/outputs/query', {})).status).toBe(404);
  });

  it('serves one output, its preview and links, with decoded ids', async () => {
    const run = '/runs/run_01HGW2wgs30x';
    await call('GET', `${run}/outputs/out6`);
    await call('GET', `${run}/outputs/${q('out6')}/preview`);
    await call('GET', `${run}/outputs/out8/view-url`);
    await call('GET', `${run}/outputs/out0/download`);
    await call('GET', `${run}/qc/general-stats`);
    const noViewer = await call('GET', `${run}/outputs/out0/view-url`);
    expect([noViewer.status, noViewer.text]).toEqual([
      409,
      'NA12878.bam cannot be shown in the browser; download it',
    ]);
    // An id with an encoded character is decoded before it is looked up.
    expect((await call('GET', `${run}/outputs/out%36/download`)).status).toBe(200);
    expect((await call('GET', `${run}/outputs/nope/preview`)).text).toBe(
      'no such output of this run',
    );
    // A preview reads the object: one that is gone (`exists: false`) is a 404 saying so.
    const gone = await call('GET', `${run}/outputs/out10/preview`);
    expect([gone.status, gone.text]).toEqual([404, "the output's object is not in S3"]);
  });

  it('lists and pages data folders, classifying files as viewer_of does', async () => {
    const grch38 = obj(await call('POST', '/data/list', { source: 'refs', path: 'GRCh38/' }));
    const viewer = Object.fromEntries(
      (grch38.files as { name: string; viewer?: string }[]).map((f) => [f.name, f.viewer ?? null]),
    );
    expect(viewer).toMatchObject({
      'genome.fa': 'text',
      'genes.gtf': 'table',
      'genome.2bit': null,
      'karyotype.svg': 'image',
    });
    const one = obj(await call('POST', '/data/list', { source: 'refs', path: 'panels/' }));
    expect(one.files as Json[]).toHaveLength(1000);
    const two = obj(
      await call('POST', '/data/list', {
        source: 'refs',
        path: 'panels/',
        cursor: one.next_cursor,
      }),
    );
    expect(two.files as Json[]).toHaveLength(50);
    expect(two.next_cursor).toBeUndefined();
    await call('POST', '/data/find', { source: 'refs', q: 'stats' });
    // As find_files: the text trimmed and in any case, in key order, cut at `limit` (truncated).
    const cut = obj(await call('POST', '/data/find', { source: 'refs', q: ' PANEL-', limit: 10 }));
    expect([(cut.files as Json[]).length, cut.truncated]).toEqual([10, true]);
    expect((cut.files as { path: string }[])[0].path).toBe('panels/panel-0001.bed');
    const all = obj(await call('POST', '/data/find', { source: 'refs', q: 'panel-', limit: 9999 }));
    expect([(all.files as Json[]).length, all.truncated]).toEqual([1050, false]);
    // One object once, though a resumed run lists the outputs it reused as well.
    const reused = obj(
      await call('POST', '/data/find', {
        source: 'project',
        project: 'imported',
        path: 'biffix/runs/run_01HGW1failed/',
        q: 'BAD01_fastqc',
      }),
    );
    expect(reused.files as Json[]).toHaveLength(1);
    // A URI no source shows (outside them all, a folder, a hidden one) is unknown, not absent.
    const heads = obj(
      await call('POST', '/data/head', {
        uris: [
          's3://shared-refs/GRCh38/genome.fa',
          's3://shared-refs/GRCh38/none.fa',
          's3://x/y',
          's3://shared-refs/GRCh38/',
          's3://genomics-project/workflow-sources/x.tar.gz',
        ],
      }),
    );
    expect((heads.objects as { exists?: boolean }[]).map((o) => o.exists)).toEqual([
      true,
      false,
      undefined,
      undefined,
      undefined,
    ]);
    const tooMany = Array.from({ length: 201 }, (_, i) => `s3://shared-refs/${i}`);
    const headCap = await call('POST', '/data/head', { uris: tooMany });
    expect([headCap.status, headCap.text]).toEqual([400, 'at most 200 uris per request']);
    await call('POST', '/data/preview', { source: 'refs', path: 'GRCh38/stats.tsv' });
    await call('POST', '/data/view-url', { source: 'refs', path: 'GRCh38/karyotype.svg' });
    await call('POST', '/data/download-url', { source: 'refs', path: 'GRCh38/genome.2bit' });
    // As file_uri: a path that names no file is a 400; the locked source refuses a read (403).
    for (const path of ['', 'GRCh38/', 'GRCh38/../x.fa']) {
      const bad = await call('POST', '/data/preview', { source: 'refs', path });
      expect([bad.status, bad.text]).toEqual([400, `\`${path}\` is not a file path`]);
    }
    expect(
      (await call('POST', '/data/preview', { source: 'refs', path: '/GRCh38/stats.tsv' })).status,
    ).toBe(200);
    expect((await call('POST', '/data/preview', { source: 'locked', path: 'a.txt' })).status).toBe(
      403,
    );
    expect((await call('POST', '/data/list', { source: 'locked' })).status).toBe(403);
    await call('POST', '/samples/detect', {
      files: [{ uri: 's3://b/P01_S1_L001_R1_001.fastq.gz' }],
    });
  });

  it('serves each project source with the hide rule the backend serves, and applies it', async () => {
    // The same literals `the_project_source_hides_biffix_prefixes` (data_sources.rs) and
    // `the_project_source_says_which_paths_it_hides` (data_api.rs) pin for the backend.
    type Src = {
      id: string;
      kind: string;
      hidden_prefixes?: string[];
      browsable_exceptions?: string[];
    };
    for (const project of ['genomics', 'imported']) {
      const sources = (await call('GET', `/data-sources?project=${project}`)).body as Src[];
      for (const s of sources) {
        if (s.kind === 'project') {
          expect(s.hidden_prefixes, project).toEqual(['biffix/', 'workflow-sources/']);
          expect(s.browsable_exceptions, project).toEqual(['biffix/runs/']);
        } else {
          expect([s.hidden_prefixes, s.browsable_exceptions], s.id).toEqual([undefined, undefined]);
        }
      }
    }
    // The mock's listing follows the rule it serves: `biffix/` is a 404, a run's folder opens.
    expect((await call('POST', '/data/list', { source: 'project', path: 'biffix/' })).status).toBe(
      404,
    );
    expect(
      (await call('POST', '/data/list', { source: 'project', path: 'biffix/runs/' })).status,
    ).toBe(200);
  });

  it("serves the deploy's projects, each with its own sources, and refuses an unknown one", async () => {
    const projects = (await call('GET', '/projects')).body as { id: string; default: boolean }[];
    expect(projects.map((p) => p.id)).toEqual(['genomics', 'imported']);
    expect(projects.filter((p) => p.default).map((p) => p.id)).toEqual(['genomics']);
    const ids = async (path: string) =>
      ((await call('GET', path)).body as { id: string }[]).map((s) => s.id);
    // Absent, and the default named, are the same project.
    expect(await ids('/data-sources?project=genomics')).toEqual(await ids('/data-sources'));
    const bench = (await call('GET', '/data-sources?project=imported')).body as {
      id: string;
      uri: string;
      writable: boolean;
    }[];
    expect(bench.map((s) => [s.id, s.uri, s.writable])).toEqual([
      ['project', 's3://imported-project/', false],
      ['bench-data', 's3://acme-imported-data/', false],
    ]);
    expect((await call('GET', '/data-sources?project=nope')).status).toBe(400);
    // The same source id lists its own project's bucket.
    const benchRoot = obj(
      await call('POST', '/data/list', { source: 'project', project: 'imported' }),
    );
    expect((benchRoot.folders as { name: string }[]).map((f) => f.name)).toEqual(['results']);
    const reads = obj(
      await call('POST', '/data/list', {
        source: 'bench-data',
        path: 'hg002/',
        project: 'imported',
      }),
    );
    expect((reads.files as Json[]).length).toBe(2);
    expect((await call('POST', '/data/list', { source: 'bench-data' })).status).toBe(404);
    // Every other read finds the project's source too (a 404 here means it looked elsewhere).
    const benchFind = await call('POST', '/data/find', {
      source: 'bench-data',
      q: 'HG002',
      project: 'imported',
    });
    expect(benchFind.status).toBe(200);
    expect((obj(benchFind).files as { name: string }[]).map((f) => f.name)).toContain(
      'HG002_R1.fastq.gz',
    );
    const benchPreview = await call('POST', '/data/preview', {
      source: 'project',
      path: 'results/summary.tsv',
      project: 'imported',
    });
    expect(benchPreview.status).toBe(200);
    expect(obj(benchPreview).kind).toBe('text');
    const benchDownload = await call('POST', '/data/download-url', {
      source: 'project',
      path: 'results/summary.tsv',
      project: 'imported',
    });
    expect(benchDownload.status).toBe(200);
    expect(typeof obj(benchDownload).url).toBe('string');
    const benchHead = await call('POST', '/data/head', {
      uris: ['s3://acme-imported-data/hg002/HG002_R1.fastq.gz', 's3://acme-imported-data/x'],
      project: 'imported',
    });
    expect(benchHead.status).toBe(200);
    expect((obj(benchHead).objects as { exists: boolean }[]).map((o) => o.exists)).toEqual([
      true,
      false,
    ]);
    for (const [path, body] of [
      ['/data/list', { source: 'project', project: 'nope' }],
      ['/data/find', { source: 'project', q: 'x', project: 'nope' }],
      ['/data/preview', { source: 'project', path: 'x', project: 'nope' }],
      ['/data/head', { uris: [], project: 'nope' }],
    ] as const)
      expect((await call('POST', path, body)).status, path).toBe(400);
  });

  it('uploads into the named project only where it allows uploads', async () => {
    const key = 'uploads/contract/project.txt';
    const start = await call('POST', '/data/uploads', { key, size: 4, project: 'genomics' });
    expect(obj(start).uri).toBe(`s3://genomics-project/${key}`);
    const id = obj(start).upload_id as string;
    // Asked in a project that takes no uploads, the call is refused before any upload is looked up.
    // (The mock has no second uploads-enabled project, so a bucket mismatch's 404 is not reachable.)
    expect(
      (await call('POST', `/data/uploads/${id}/status`, { key, project: 'imported' })).status,
    ).toBe(403);
    expect(
      (await call('POST', `/data/uploads/${id}/status`, { key, project: 'genomics' })).status,
    ).toBe(200);
    const off = await call('POST', '/data/uploads', { key, size: 4, project: 'imported' });
    expect([off.status, off.text]).toEqual([
      403,
      "browser uploads are not enabled for this project's bucket",
    ]);
    expect((await call('POST', '/data/uploads', { key, size: 4, project: 'nope' })).status).toBe(
      400,
    );
    await call('POST', `/data/uploads/${id}/abort`, { key });
  });

  it('checks every upload call against the key it names', async () => {
    const key = 'uploads/contract/reads.txt';
    const start = obj(await call('POST', '/data/uploads', { key, size: 10 }));
    const id = start.upload_id as string;
    const parts = obj(await call('POST', `/data/uploads/${id}/parts`, { key, part_numbers: [1] }));
    const url = (parts.urls as { url: string }[])[0].url;
    expect(url.startsWith(origin)).toBe(true);
    const put = await fetch(url, { method: 'PUT', body: '0123456789' });
    const etag = put.headers.get('etag')!.replaceAll('"', '');
    await call('POST', `/data/uploads/${id}/status`, { key });
    // The key is required, must be the upload's, and at most 100 part URLs come at a time.
    expect((await call('POST', `/data/uploads/${id}/parts`, { part_numbers: [1] })).status).toBe(
      422,
    );
    expect(
      (await call('POST', `/data/uploads/${id}/status`, { key: 'uploads/other' })).status,
    ).toBe(404);
    const many = Array.from({ length: 101 }, (_, i) => i + 1);
    const tooMany = await call('POST', `/data/uploads/${id}/parts`, { key, part_numbers: many });
    expect([tooMany.status, tooMany.text]).toEqual([400, 'ask for 1–100 parts at a time']);
    // A part S3 took but the complete does not list is not in the object.
    const extra = obj(await call('POST', `/data/uploads/${id}/parts`, { key, part_numbers: [2] }));
    await fetch((extra.urls as { url: string }[])[0].url, { method: 'PUT', body: 'unlisted' });
    const done = await call('POST', `/data/uploads/${id}/complete`, {
      key,
      parts: [{ part_number: 1, etag }],
    });
    const head = obj(await call('POST', '/data/head', { uris: [obj(done).uri] }));
    expect((head.objects as { size: number }[])[0].size).toBe(10);
    // `size` is required; the parts grow so a file needs at most 10,000; 5 TiB is the most.
    const noSize = await call('POST', '/data/uploads', { key: 'uploads/c/nosize.txt' });
    expect([noSize.status, noSize.text]).toEqual([
      422,
      'Failed to deserialize the JSON body into the target type: missing field `size`',
    ]);
    const big = obj(
      await call('POST', '/data/uploads', { key: 'uploads/c/big.bam', size: 20 * 1024 ** 3 }),
    );
    expect([big.part_size, big.parts]).toEqual([3 * 1024 ** 2, 6827]);
    const huge = await call('POST', '/data/uploads', {
      key: 'uploads/c/huge.bam',
      size: 5 * 1024 ** 4 + 1,
    });
    expect([huge.status, huge.text]).toEqual([400, 'a file may be at most 5 TiB']);
    for (const bad of ['uploads/a//b', 'uploads/./x', 'uploads/a/../b', 'uploads/', 'x/y'])
      expect((await call('POST', '/data/uploads', { key: bad, size: 1 })).status, bad).toBe(400);
    expect((await call('POST', '/data/uploads', { key: 'uploads/a..b', size: 1 })).status).toBe(
      201,
    );
    // Two uploads to one key: the second's complete is refused (409), and, as S3 leaves it, its
    // upload stays open.
    const raceKey = 'uploads/c/race.txt';
    const finish = async (id: string, expected: number) => {
      const urls = obj(
        await call('POST', `/data/uploads/${id}/parts`, { key: raceKey, part_numbers: [1] }),
      );
      const putPart = await fetch((urls.urls as { url: string }[])[0].url, {
        method: 'PUT',
        body: 'x',
      });
      const partEtag = putPart.headers.get('etag')!.replaceAll('"', '');
      const parts = [{ part_number: 1, etag: partEtag }];
      expect(
        (await call('POST', `/data/uploads/${id}/complete`, { key: raceKey, parts })).status,
      ).toBe(expected);
    };
    const first = obj(await call('POST', '/data/uploads', { key: raceKey, size: 1 }));
    const second = obj(await call('POST', '/data/uploads', { key: raceKey, size: 1 }));
    await finish(first.upload_id as string, 200);
    await finish(second.upload_id as string, 409);
    expect(
      (await call('POST', `/data/uploads/${second.upload_id}/status`, { key: raceKey })).status,
    ).toBe(200);
    const abort = obj(await call('POST', '/data/uploads', { key: 'uploads/c/abort.txt', size: 1 }));
    await call('POST', `/data/uploads/${abort.upload_id}/abort`, { key: 'uploads/c/abort.txt' });
  });

  it('keeps the caller’s preferences as the backend does', async () => {
    expect(obj(await call('GET', '/me/prefs'))).toEqual({ pinned_workflows: [] });
    const prefs = obj(await call('PUT', '/me/prefs', { view: 'expert', pin: ' x ' }));
    expect(prefs).toEqual({ view: 'expert', pinned_workflows: ['x'] });
    expect(obj(await call('GET', '/me/prefs')).view).toBe('expert');
    await call('PUT', '/me/prefs', { view: 'lab', unpin: 'x' });
    // Pins are operations on the stored list: both of two pins are kept, an unpin removes one.
    await call('PUT', '/me/prefs', { pin: 'a' });
    expect(obj(await call('PUT', '/me/prefs', { pin: 'b' })).pinned_workflows).toEqual(['a', 'b']);
    expect(obj(await call('PUT', '/me/prefs', { unpin: 'a' })).pinned_workflows).toEqual(['b']);
    // A refused update writes none of itself, its view included (me.rs checks the whole update
    // before the store writes it).
    const refusals: [Record<string, Json>, string][] = [
      [
        { view: 'expert', pin: 'c', unpin: 'b' },
        'give at most one of `pin` and `unpin` in one update',
      ],
      [{ view: 'expert', pin: '  ' }, 'pin: `  ` must be 1 to 512 characters'],
      [{ view: 'expert', unpin: '' }, 'unpin: `` must be 1 to 512 characters'],
    ];
    for (const [body, message] of refusals) {
      const bad = await call('PUT', '/me/prefs', body);
      expect([bad.status, bad.text]).toEqual([400, message]);
    }
    expect(obj(await call('GET', '/me/prefs'))).toEqual({ view: 'lab', pinned_workflows: ['b'] });
    // A pin past the cap is refused, and nothing else in its update is written.
    const more = Array.from({ length: 49 }, (_, i) => `p${i}`);
    for (const pin of more) await call('PUT', '/me/prefs', { pin });
    const full = await call('PUT', '/me/prefs', { view: 'expert', pin: 'one more' });
    expect([full.status, full.text]).toEqual([400, 'pin: at most 50 pinned pipelines']);
    expect(obj(await call('GET', '/me/prefs')).view).toBe('lab');
    for (const unpin of ['b', ...more]) await call('PUT', '/me/prefs', { unpin });
    expect(obj(await call('GET', '/me/prefs')).pinned_workflows).toEqual([]);
  });

  it('adds and removes alert recipients as settings.rs does', async () => {
    const emails = async () =>
      list<{ email: string; status: string }>(await call('GET', '/settings/alerts'), 'recipients');
    const start = await emails();
    expect(start.map((r) => r.email)).toEqual(['oncall@lab.example.org', 'ops@lab.example.org']);
    const bad = await call('POST', '/settings/alerts', { email: 'not an address' });
    expect([bad.status, bad.text]).toEqual([
      400,
      'not a valid email address: "not an address" (one `@`, a dotted domain, no spaces, and none of `? & # % , < > =` (unsafe in a mail link))',
    ]);
    // Trimmed; an address already there in another case is a no-op; listed by lower-cased email.
    await call('POST', '/settings/alerts', { email: '  Lead@Lab.example.org ' });
    await call('POST', '/settings/alerts', { email: 'lead@lab.example.org' });
    expect((await emails()).map((r) => r.email)).toEqual([
      'Lead@Lab.example.org',
      'oncall@lab.example.org',
      'ops@lab.example.org',
    ]);
    const unknown = await call('POST', '/settings/alerts/remove', {
      email: 'nobody@lab.example.org',
    });
    expect([unknown.status, unknown.text]).toEqual([
      404,
      'nobody@lab.example.org is not a recipient',
    ]);
    // A pending recipient cannot be removed until it confirms.
    const pending = await call('POST', '/settings/alerts/remove', {
      email: 'LEAD@lab.example.org',
    });
    expect(pending.status).toBe(409);
    expect(
      (await call('POST', '/settings/alerts/remove', { email: ' OPS@lab.example.org' })).status,
    ).toBe(204);
    expect((await emails()).map((r) => r.email)).toEqual([
      'Lead@Lab.example.org',
      'oncall@lab.example.org',
    ]);
  });

  it('checks a settings update whole before writing any of it', async () => {
    const before = obj(await call('GET', '/settings'));
    const refusals: [Record<string, Json>, string][] = [
      [
        { title: 'Changed', support: { name: 'Ana', email: 'not-an-email' } },
        'support.email: `not-an-email` is not a valid email address (one `@`, a dotted domain, no spaces, and none of `? & # % , < > =` (unsafe in a mail link))',
      ],
      [
        { title: 'Changed', support: { name: ' ', email: 'a@example.org' } },
        'support.name: must be 1 to 100 characters',
      ],
      [
        { title: 'Changed', support: { name: 'Ana', email: 'a@example.org', url: 'ftp://x' } },
        'support.url: must be an http:// or https:// URL',
      ],
      [{ title: 'x'.repeat(101) }, 'title must be at most 100 characters'],
      [
        { title: 'Changed', logo_data_url: 'data:image/svg+xml;base64,PHN2Zy8+' },
        'logo must be one of image/png, image/jpeg, image/gif, image/webp (got `image/svg+xml`)',
      ],
      [
        { title: 'Changed', logo_data_url: 'data:image/png;base64,AAAA' },
        "logo's data is not a image/png image",
      ],
    ];
    for (const [body, message] of refusals) {
      const bad = await call('PUT', '/settings', body);
      expect([bad.status, bad.text]).toEqual([400, message]);
    }
    expect(obj(await call('GET', '/settings'))).toEqual(before);
    // A one-pixel PNG is a logo; a blank title clears the title.
    const png =
      'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';
    const saved = obj(await call('PUT', '/settings', { title: '  ', logo_data_url: png }));
    expect([saved.title, saved.logo_data_url]).toEqual([null, png]);
    await call('PUT', '/settings', { title: before.title, logo_data_url: null });
  });

  it('lists the catalog and hides a pipeline as the backend does', async () => {
    const list = (await call('GET', '/workflows')).body as Record<string, Json>[];
    const rnaseq = list.find((w) => w.name === 'nf-core/rnaseq')!;
    expect(rnaseq.catalog).toMatchObject({ tags: ['RNA-seq', 'QC'], verified: true });
    expect(rnaseq.hidden).toBe(false);
    // A version with runs carries its newest run's time; one without, none.
    for (const w of list)
      expect(typeof w.last_run_at).toBe((w.run_count as number) > 0 ? 'number' : 'undefined');
    const body = { project: 'imported', name: 'umi-consensus', hidden: true };
    expect(obj(await call('PUT', '/workflows/listing', body)).hidden).toBe(true);
    const hidden = (await call('GET', '/workflows')).body as Record<string, Json>[];
    expect(hidden.find((w) => w.name === 'umi-consensus')!.hidden).toBe(true);
    await call('PUT', '/workflows/listing', { ...body, hidden: false });
    const unknown = { project: 'x', name: 'nope', hidden: true };
    expect((await call('PUT', '/workflows/listing', unknown)).status).toBe(404);
  });

  it('releases and un-releases a version as the backend does, most versions developer ones', async () => {
    type Release = { released: boolean; at_micros?: number; by?: string; by_label?: string };
    const list = (await call('GET', '/workflows')).body as { id: string; release: Release }[];
    const released = list.filter((w) => w.release.released).map((w) => w.id);
    expect(released.sort()).toEqual(['112233445566:v2', 'aabbccddeeff:v1']);
    // A developer version's release is just `released: false`.
    expect(list.find((w) => !w.release.released)!.release).toEqual({ released: false });
    // `by` is the bare subject the backend records, not the submitter form.
    expect(list.find((w) => w.id === 'aabbccddeeff:v1')!.release.by).toBe('alice');
    const onlyReleased = (await call('GET', '/workflows?released=true')).body as { id: string }[];
    expect(onlyReleased.map((w) => w.id).sort()).toEqual(released.sort());
    // Like `Query<ListWorkflowsQuery>`: only `true` / `false`.
    expect((await call('GET', '/workflows?released=yes')).status).toBe(400);
    const dev = '7a8b9c0d1e2f:dev1';
    const before = obj(await call('GET', `/workflows/${q(dev)}`));
    expect(before.release).toEqual({ released: false });
    // Like `Json<WorkflowReleaseUpdateRequest>`: `released` is a required boolean.
    for (const body of [{}, { released: 'yes' }])
      expect((await call('PUT', `/workflows/${q(dev)}/release`, body)).status).toBe(422);
    const after = obj(await call('PUT', `/workflows/${q(dev)}/release`, { released: true }));
    expect((after.release as Release).released).toBe(true);
    expect((after.release as Release).by).toBe('alice');
    // The same change again records nothing.
    const again = obj(await call('PUT', `/workflows/${q(dev)}/release`, { released: true }));
    expect((again.history as Json[]).length).toBe(1);
    const detail = obj(await call('GET', `/workflows/${q(dev)}`));
    expect((detail.release as Release).released).toBe(true);
    expect(detail.can_release).toBe(true);
    const back = obj(await call('PUT', `/workflows/${q(dev)}/release`, { released: false }));
    expect([back.release, (back.history as Json[]).length]).toEqual([{ released: false }, 2]);
    expect(
      (await call('PUT', `/workflows/${q('nope:v1')}/release`, { released: true })).status,
    ).toBe(404);
    // Every version GET /workflows/{id} serves as registered has a release: one reached only by
    // id, the Lab's, and one only a run mentions.
    const runs = obj(await call('POST', '/runs/query', {})).runs as { workflow: string }[];
    const runOnly = runs.map((r) => r.workflow).find((id) => id && !list.some((w) => w.id === id))!;
    expect(runOnly).toBeDefined();
    for (const id of ['ccddeeff0011:pending', 'c4d5e6f7a8b9:v1', runOnly]) {
      expect(obj(await call('GET', `/workflows/${q(id)}`)).can_release).toBe(true);
    }
    // A run of it says when it was registered, as the version's detail does.
    const ofRunOnly = (runs as { run_id: string; workflow: string }[]).find(
      (r) => r.workflow === runOnly,
    )!;
    const registered = obj(await call('GET', `/workflows/${q(runOnly)}`)).created_at_micros;
    expect(obj(await call('GET', `/runs/${ofRunOnly.run_id}`)).workflow_registered_at_micros).toBe(
      registered,
    );
  });

  it('serves a run’s samples grid', async () => {
    const grid = obj(await call('GET', '/runs/run_01HGW3align/samples'));
    const samples = grid.samples as { id: string; state: string }[];
    expect(samples.map((s) => s.id)).toEqual(['HG00096', 'HG00097']);
    expect((grid.steps as { group: string }[]).map((s) => s.group)).toEqual([
      'fastqc',
      'align',
      'markdup',
      'joint_call',
    ]);
    expect((await call('GET', '/runs/nope/samples')).status).toBe(404);
    // Every run-scoped read loads the run first (load_visible_run): an unknown one is a bare 404.
    for (const read of ['provenance', 'reports', 'qc/general-stats', 'qc/fastqc-status']) {
      const missing = await call('GET', `/runs/nope/${read}`);
      expect([missing.status, missing.text], read).toEqual([404, '']);
    }
    expect((await call('GET', `/workflows/${q('nope:v1')}/estimate`)).status).toBe(404);
  });

  it('keeps settings, alerts and reference bundles as the backend does', async () => {
    await call('PUT', '/settings', { title: 'contract' });
    await call('GET', '/settings/alerts');
    await call('POST', '/settings/alerts', { email: 'new@lab.example.org' });
    await call('POST', '/settings/alerts/remove', { email: 'new@lab.example.org' });
    await call('GET', '/references');
    await call('GET', '/references?kind=assembly&keys=fasta,gtf');
    const bundle = {
      id: 'z-contract',
      kind: 'assembly',
      display_name: 'Contract',
      uris: { fasta: 's3://shared-refs/x.fa' },
    };
    const created = await call('POST', '/references', bundle);
    expect([created.status, obj(created).bundle]).toMatchObject([201, { created_by: 'alice' }]);
    // Checked as validate_request checks it, before the id clash: any project's sources hold a
    // bundle's files, but not biffix's own prefixes.
    const refusals: [Record<string, Json>, string][] = [
      [
        { ...bundle, uris: { fasta: 's3://elsewhere/x.fa' } },
        'fasta: `s3://elsewhere/x.fa` is not in the project bucket or a registered data source',
      ],
      [
        { ...bundle, uris: { fasta: 's3://genomics-project/biffix/runs/r/x.fa' } },
        'fasta: `s3://genomics-project/biffix/runs/r/x.fa` is under a prefix biffix reserves for its own files',
      ],
      [
        { ...bundle, uris: { fasta: 's3://shared-refs/' } },
        'fasta: `s3://shared-refs/` is not an s3://bucket/key URI',
      ],
      [{ ...bundle, uris: {} }, 'a bundle needs at least one URI'],
      [{ ...bundle, kind: 'pangenome' }, 'kind must be one of assembly, assay_kit'],
      [
        { ...bundle, id: 'Z' },
        'id `Z` must be 1–64 lowercase letters, digits, `-`, `_` or `.`, starting with a letter or digit',
      ],
    ];
    for (const [body, message] of refusals) {
      const bad = await call('POST', '/references', body);
      expect([bad.status, bad.text]).toEqual([400, message]);
    }
    const imported = {
      ...bundle,
      id: 'z-imported',
      uris: { fasta: 's3://acme-imported-data/x.fa' },
    };
    expect((await call('POST', '/references', imported)).status).toBe(201);
    expect((await call('DELETE', '/references/z-imported')).status).toBe(204);
    await call('POST', '/references', { ...bundle, id: 'a_contract' });
    await call('PUT', `/references/${bundle.id}`, bundle);
    const renamed = await call('PUT', `/references/${bundle.id}`, { ...bundle, id: 'other' });
    expect([renamed.status, renamed.text]).toEqual([
      400,
      "a bundle's id cannot change; add a new bundle instead",
    ]);
    expect((await call('PUT', '/references/none', { ...bundle, id: 'none' })).status).toBe(404);
    const all = (await call('GET', '/references')).body as { id: string }[];
    const order = all.map((b) => b.id);
    expect(order).toEqual([...order].sort());
    expect((await call('DELETE', `/references/${bundle.id}`)).status).toBe(204);
  });

  it('serves the workflow versions, their details and estimates', async () => {
    const versions = (await call('GET', '/workflows')).body as { id: string }[];
    const ids = [
      ...versions.map((v) => v.id),
      'ccddeeff0011:pending',
      'ddeeff001122:deriving',
      'eeff00112233:building',
      'aabbccddeeff:smk1',
    ];
    // Each names its engine's entrypoint (a Snakemake version is never `main.nf`).
    for (const id of ids) {
      const detail = obj(await call('GET', `/workflows/${q(id)}`));
      const entrypoint = detail.engine === 'snakemake' ? 'Snakefile' : 'main.nf';
      expect(detail.entrypoint, id).toBe(entrypoint);
    }
    await call('GET', `/workflows/${q('aabbccddeeff:v1')}/estimate?samples=3`);
    // A typical run alone (no samples), with a cost and without one.
    const typical = obj(await call('GET', `/workflows/${q('aabbccddeeff:v1')}/estimate`));
    expect(typical.estimate).toBeUndefined();
    expect(typical.typical).toMatchObject({ hours_median: 2.5, runs: 6 });
    const unpriced = obj(await call('GET', `/workflows/${q('ddeeff001122:v3')}/estimate`));
    expect(unpriced.typical).toEqual({ hours_median: 0.5, runs: 3 });
    const noVersion = await call('GET', `/workflows/${q('nope:v1')}`);
    expect([noVersion.status, noVersion.text]).toEqual([404, '']);
  });

  it("launches a run from its version and request, never another run's facts", async () => {
    const launched = obj(
      await call('POST', '/runs', { workflow_version_id: 'aabbccddeeff:v1', params: {} }),
    );
    const run = obj(await call('GET', `/runs/${launched.run_id}`));
    expect(run).toMatchObject({
      workflow: 'aabbccddeeff:v1',
      engine: 'nextflow',
      state: 'submitted',
      tasks_observed: 0,
      completed: null,
      total: null,
      prefix_kind: 'isolated',
      output_prefix: `s3://genomics-project/biffix/runs/${launched.run_id}/`,
    });
    expect(run.cost).toBeUndefined();
    const listed = list(await call('POST', '/runs/query', { limit: 200 }), 'runs');
    expect(listed.find((r) => r.run_id === launched.run_id)).toBeDefined();

    const resumed = obj(
      await call('POST', '/runs', {
        workflow_version_id: 'ddeeff001122:v3',
        params: {},
        resume_from: 'run_01HGW4resume',
      }),
    );
    expect(obj(await call('GET', `/runs/${resumed.run_id}`))).toMatchObject({
      prefix_kind: 'custom',
      output_prefix: 's3://genomics-project/biffix/runs/run_01HGW0cancel/',
      engine_session_id: '0108f2c4-f01e-46f0-b29c-28f12803e2db',
      resumed_from: 'run_01HGW4resume',
    });
    const withImage = obj(
      await call('POST', '/runs', {
        workflow_version_id: 'aabbccddeeff:v1',
        params: {},
        images: ['ghcr.io/lab/x:1'],
      }),
    );
    expect(withImage.state).toBe('preparing_images');
    await call('GET', `/runs/${withImage.run_id}`);
    expect(
      (await call('POST', '/runs', { workflow_version_id: 'nope:v1', params: {} })).status,
    ).toBe(404);
    const building = await call('POST', '/runs', {
      workflow_version_id: 'ccddeeff0011:pending',
      params: {},
    });
    expect(building.status).toBe(409);

    // Refused as submit refuses it, and trimmed as submit records it.
    const rnaseq = { workflow_version_id: 'aabbccddeeff:v1', params: {} };
    const refusals: [Record<string, Json>, number, string][] = [
      [{ ...rnaseq, run_name: 'x'.repeat(201) }, 400, 'run_name must be at most 200 characters'],
      [{ ...rnaseq, params: [] }, 400, 'params must be a JSON object'],
      [{ ...rnaseq, retries: 11 }, 400, 'retries must be at most 10'],
      [{ ...rnaseq, reference: 'nope' }, 400, 'unknown reference bundle nope'],
      [
        { workflow_version_id: 'ddeeff001122:v3', params: {}, reference: 'grcm39' },
        400,
        'this workflow version takes no reference bundle',
      ],
      [{ ...rnaseq, resume_from: 'nope' }, 404, '`resume_from`: unknown run nope'],
      [{ ...rnaseq, retry_of: ' nope ' }, 404, '`retry_of`: unknown run nope'],
      [
        { ...rnaseq, resume_from: 'run_01HGW8nflive' },
        400,
        'run run_01HGW8nflive is a run of genomics/nf-core/sarek, not genomics/nf-core/rnaseq',
      ],
      [
        { workflow_version_id: 'ddeeff001122:v3', params: {}, resume_from: 'run_01HGW8nflive' },
        400,
        'run run_01HGW8nflive is still running; only a finished run can be resumed',
      ],
      [
        {
          workflow_version_id: 'ddeeff001122:v3',
          params: {},
          resume_from: 'run_01HGW4resume',
          output_prefix: 's3://genomics-project/elsewhere/',
        },
        400,
        "output_prefix must equal the resumed run's (s3://genomics-project/biffix/runs/run_01HGW0cancel/)",
      ],
    ];
    for (const [body, status, message] of refusals) {
      const bad = await call('POST', '/runs', body);
      expect([bad.status, bad.text], JSON.stringify(body)).toEqual([status, message]);
    }
    const named = obj(
      await call('POST', '/runs', { ...rnaseq, run_name: '  My run ', experiment: '  ' }),
    );
    const namedRun = obj(await call('GET', `/runs/${named.run_id}`));
    expect([namedRun.run_name, namedRun.experiment_id, namedRun.submitter]).toEqual([
      'My run',
      undefined,
      'user:alice',
    ]);
  });

  it('cancels a running run once its coordinator reports, a not-yet-running one at once', async () => {
    expect((await call('POST', '/runs/run_01HGW5cancelme/cancel')).status).toBe(202);
    // The request is recorded, but the run is still running until its coordinator's FAILED is
    // folded in (the mock folds it on the run's next read).
    const listed = list(await call('POST', '/runs/query', { limit: 200 }), 'runs');
    expect(listed.find((r) => r.run_id === 'run_01HGW5cancelme')?.state).toBe('running');
    expect(obj(await call('GET', '/runs/run_01HGW5cancelme')).state).toBe('cancelled');
    await call('POST', '/runs/run_01HGW7preparing/cancel');
    expect(obj(await call('GET', '/runs/run_01HGW7preparing')).state).toBe('cancelled');
    const unknown = await call('POST', '/runs/nope/cancel');
    expect([unknown.status, unknown.text]).toEqual([404, 'unknown run']);
  });

  it('lists the caller’s experiments on ?mine=true, each marked hers and counting her analyses that need a look', async () => {
    type Exp = { id: string; created_by?: string; needs_attention?: number; mine?: boolean };
    const mine = async (query = '') =>
      list<Exp>(await call('GET', `/experiments?mine=true${query}`), 'experiments');
    const ids = async (query = '') => (await mine(query)).map((e) => e.id);
    // Others' experiments holding none of her analyses are not hers; an archived one she made is,
    // on asking for archived ones.
    expect(await ids()).not.toContain('exp_liver_tumor');
    expect(await ids()).not.toContain('exp_pilot_2025');
    expect(await ids('&archived=true')).toContain('exp_pilot_2025');
    expect(await ids('&project=nowhere')).toEqual([]);
    // One she makes is hers.
    const made = obj(
      await call('POST', '/experiments', { project: 'genomics', name: 'Mine, C10' }),
    );
    expect(await ids()).toContain(made.id);
    expect(made.needs_attention).toBe(0);
    // As the backend records it: who made it is her bare id, not a run's `user:` submitter.
    expect(made.created_by).toBe('alice');
    expect(made.mine).toBe(true);
    // Everyone's list marks which are hers, as `?mine=true` keeps them.
    const everyone = list<Exp>(await call('GET', '/experiments?archived=true'), 'experiments');
    expect(everyone.filter((e) => e.mine).map((e) => e.id)).toEqual(await ids('&archived=true'));
    expect(everyone.find((e) => e.id === 'exp_liver_tumor')?.mine).toBe(false);

    // An analysis of hers that needs a look makes another's experiment hers, and counts there.
    const failing = list<{ run_id: string; project?: string; experiment_id?: string | null }>(
      await call('POST', '/runs/query', { attention: true }),
      'runs',
    ).find((r) => r.project === 'genomics' || r.project === 'imported')!;
    const theirs = failing.project === 'genomics' ? 'exp_liver_tumor' : 'exp_umi_tuning';
    const before = (await mine()).length;
    const member = (experiment_id: string | null) =>
      call('POST', `/runs/${q(failing.run_id)}/experiment`, { experiment_id });
    expect((await member(theirs)).status).toBe(200);
    const joined = (await mine()).find((e) => e.id === theirs);
    expect(joined?.needs_attention).toBe(1);
    expect((await mine()).length).toBe(before + 1);
    expect(obj(await call('GET', `/experiments/${theirs}`)).experiment).toMatchObject({
      needs_attention: 1,
    });
    // Everyone's list carries the same count.
    expect(
      list<Exp>(await call('GET', '/experiments'), 'experiments').find((e) => e.id === theirs)
        ?.needs_attention,
    ).toBe(1);
    // Taken out again, it is not hers.
    expect((await member(null)).status).toBe(200);
    expect(await ids()).not.toContain(theirs);

    // Leave the shared mock as found: the run back in the experiment it was in, and the one made
    // here archived (there is no delete).
    expect((await member(failing.experiment_id ?? null)).status).toBe(200);
    expect(
      (await call('PUT', `/experiments/${q(String(made.id))}`, { archived: true })).status,
    ).toBe(200);
  });

  it('keeps the caller’s notifications and their read state as the backend does', async () => {
    const notes = () => call('GET', '/me/notifications', undefined).then(obj);
    const read = (body: unknown) => call('POST', '/me/notifications/read', body);
    const attention = async () =>
      obj(await call('POST', '/runs/query', { attention: true, limit: 200 }));

    // One per ended run of hers, newest first; the counts are over all of them.
    const first = await notes();
    const items = first.items as { run_id: string; kind: string; at: number; read: boolean }[];
    expect(items.length).toBeGreaterThan(0);
    expect(items.map((n) => n.at)).toEqual([...items.map((n) => n.at)].sort((a, b) => b - a));
    expect(first.unread).toBe(items.filter((n) => !n.read).length);
    const unreadFailed = items.filter((n) => !n.read && n.kind === 'failed');
    expect(first.unread_failed).toBe(unreadFailed.length);
    expect(unreadFailed.map((n) => n.run_id)).toContain('run_01HGWAimgfail');
    // The backend's ids, `<run id>:<state>`.
    for (const n of items as unknown as { id: string; run_id: string; kind: string }[])
      expect(n.id).toBe(`${n.run_id}:${n.kind}`);
    expect(
      (
        (await call('GET', '/me/notifications?limit=1', undefined)).body as {
          items: Json[];
        }
      ).items,
    ).toHaveLength(1);

    // Needs attention: her failed runs not dismissed (none yet), with the true total.
    const needs = await attention();
    expect(needs.total).toBe(first.unread_failed);
    expect((needs.runs as { state: string }[]).every((r) => r.state === 'failed')).toBe(true);
    const ids = (l: Record<string, Json>) => (l.runs as { run_id: string }[]).map((r) => r.run_id);

    // Exactly one of ids, run_ids or all.
    expect((await read({})).status).toBe(400);
    expect((await read({ all: true, ids: [] })).status).toBe(400);
    expect((await read({ all: false })).status).toBe(400);
    // Reading (opening) a failed run leaves it needing attention.
    const after = obj(await read({ run_ids: ['run_01HGWAimgfail'] }));
    expect(after.unread_failed).toBe(Number(first.unread_failed) - 1);
    expect((await attention()).total).toBe(needs.total);
    expect(ids(await attention())).toContain('run_01HGWAimgfail');

    // Dismissing takes out exactly the named runs (not someone else's, not one that did not fail),
    // and reads nothing.
    const dismiss = (body: unknown) => call('POST', '/me/attention/dismiss', body);
    expect((await dismiss({ run_ids: [] })).status).toBe(400);
    expect((await dismiss({ run_ids: ['x'.repeat(3)].concat(Array(500).fill('y')) })).status).toBe(
      400,
    );
    // A run that did not fail (and an unknown one) is not taken: the count stays.
    expect(obj(await dismiss({ run_ids: ['run_01HGWBsubstr', 'nope'] }))).toEqual({
      attention: needs.total,
    });

    // Trying a run again reads it (and dismisses it, were it failed); an unknown run is refused.
    const retry = { workflow_version_id: 'aabbccddeeff:v1', params: {} };
    const readOf = async (run: string) =>
      ((await notes()).items as { run_id: string; read: boolean }[]).find((n) => n.run_id === run)
        ?.read;
    expect(await readOf('run_01HGWBsubstr')).toBe(false);
    expect((await call('POST', '/runs', { ...retry, retry_of: 'nope' })).status).toBe(404);
    await call('POST', '/runs', { ...retry, retry_of: 'run_01HGWBsubstr' });
    expect(await readOf('run_01HGWBsubstr')).toBe(true);
    // A failed run still in Needs attention, tried again: it leaves it.
    expect(ids(await attention())).toContain('run_01HGWAimgfail');
    await call('POST', '/runs', { ...retry, retry_of: 'run_01HGWAimgfail' });
    const afterRetry = await attention();
    expect(ids(afterRetry)).not.toContain('run_01HGWAimgfail');
    expect(afterRetry.total).toBe(Number(needs.total) - 1);
    // Dismissing it after changes nothing (its first dismissal stands) and reads nothing more.
    const unreadBefore = (await notes()).unread_failed;
    expect(obj(await dismiss({ run_ids: ['run_01HGWAimgfail'] }))).toEqual({
      attention: Number(needs.total) - 1,
    });
    expect((await notes()).unread_failed).toBe(unreadBefore);

    expect(obj(await read({ all: true }))).toEqual({ unread: 0, unread_failed: 0 });
  });

  it('says who cancelled a run only once it ended cancelled, and leaves no unread notification of one’s own', async () => {
    const launched = obj(
      await call('POST', '/runs', { workflow_version_id: 'aabbccddeeff:v1', params: {} }),
    );
    const id = String(launched.run_id);
    expect(obj(await call('GET', `/runs/${id}`))).not.toHaveProperty('cancelled_by');
    expect((await call('POST', `/runs/${id}/cancel`)).status).toBe(202);
    const ended = obj(await call('GET', `/runs/${id}`));
    expect([ended.state, ended.cancelled_by]).toEqual(['cancelled', 'alice@lab.example.org']);
    // She cancelled it herself: its notification is there, already read.
    const items = obj(await call('GET', '/me/notifications?limit=100', undefined)).items as {
      id: string;
      run_id: string;
      read: boolean;
    }[];
    expect(items.find((n) => n.run_id === id)).toMatchObject({ id: `${id}:cancelled`, read: true });
  });

  it('keeps experiments, their runs and rollups as the backend does', async () => {
    type Exp = {
      id: string;
      name: string;
      project: string;
      run_count: number;
      archived_at?: number;
    };
    const listOf = async (query = '') =>
      list<Exp>(await call('GET', `/experiments${query}`), 'experiments');

    // Archived ones only on asking; a project scopes.
    const open = await listOf();
    expect(open.every((e) => e.archived_at == null)).toBe(true);
    expect((await listOf('?archived=true')).length).toBeGreaterThan(open.length);
    expect((await listOf('?project=genomics')).every((e) => e.project === 'genomics')).toBe(true);
    const liver = open.find((e) => e.id === 'exp_liver_tumor')!;
    expect(liver.run_count).toBe(2);

    // The detail: its runs newest first, every sample's state in each, one total cost.
    const detail = obj(await call('GET', '/experiments/exp_liver_tumor'));
    const runs = detail.runs as { run_id: string; submitted_at: number; experiment_id: string }[];
    expect(runs.map((r) => r.run_id)).toEqual(['run_01HGWFbadin', 'run_01HGWDdone']);
    expect(runs.every((r) => r.experiment_id === 'exp_liver_tumor')).toBe(true);
    const samples = detail.samples as { id: string; by_run: Record<string, string> }[];
    expect(samples.find((s) => s.id === 'Normal_2')?.by_run).toMatchObject({
      run_01HGWDdone: 'done',
      run_01HGWFbadin: 'failed',
    });
    // Every run counted, and only priced ones in the total (batch-12's jobs are not priced yet).
    expect(detail.runs_total).toBe(2);
    expect(detail.total_cost_usd).toBe(13.8);
    expect((await call('GET', '/experiments/nope')).status).toBe(404);

    // Its samples as CSV: a header, then one row per sample per run, runs oldest first, named as
    // the server names it.
    const csvHead = await fetch(`${origin}/experiments/exp_liver_tumor/samples.csv`);
    expect(csvHead.headers.get('content-disposition')).toBe(
      'attachment; filename="Liver tumor study-samples.csv"',
    );
    const csv = await call('GET', '/experiments/exp_liver_tumor/samples.csv');
    // (`Response.text()` drops the UTF-8 byte-order mark the file starts with.)
    expect(csv.text.startsWith('Sample,Sample status,Reused,Analysis,Run ID,')).toBe(true);
    const rows = csv.text.trimEnd().split('\r\n').slice(1);
    expect(rows.length).toBe(samples.reduce((n, s) => n + Object.keys(s.by_run).length, 0));
    expect(rows[0]).toContain(',run_01HGWDdone,');
    expect(rows[rows.length - 1]).toContain(',run_01HGWFbadin,');
    expect((await call('GET', '/experiments/nope/samples.csv')).status).toBe(404);
    // Named in the downloader's time zone; one the server does not know is a 400 naming it.
    expect(
      (await call('GET', '/experiments/exp_liver_tumor/samples.csv?tz=Europe%2FBerlin')).status,
    ).toBe(200);
    const badZone = await call('GET', '/experiments/exp_liver_tumor/samples.csv?tz=Mars%2FOlympus');
    expect(badZone.status).toBe(400);
    expect(badZone.text).toBe(
      'unknown time zone `Mars/Olympus` (an IANA name, such as `Europe/Berlin`)',
    );

    // The runs list filters on it.
    const filtered = obj(await call('POST', '/runs/query', { experiment: 'exp_liver_tumor' }));
    expect(filtered.total).toBe(2);
    // Trimmed, as the backend reads it.
    expect(obj(await call('POST', '/runs/query', { experiment: ' exp_liver_tumor ' })).total).toBe(
      2,
    );

    // Create: the name trimmed, unique ignoring case among the project's open experiments.
    const create = (body: unknown) => call('POST', '/experiments', body);
    // The backend's words: the clash names the name asked for.
    const clash = await create({ project: 'genomics', name: ' liver TUMOR study ' });
    expect([clash.status, clash.text]).toEqual([
      409,
      'project genomics already has an experiment named `liver TUMOR study`',
    ]);
    for (const [name, words] of [
      ['   ', 'an experiment name must not be blank'],
      ['x'.repeat(201), 'an experiment name is 201 characters; the limit is 200'],
      // Characters, not UTF-16 units.
      ['🧬'.repeat(201), 'an experiment name is 201 characters; the limit is 200'],
    ]) {
      const bad = await create({ project: 'genomics', name });
      expect([bad.status, bad.text]).toEqual([400, words]);
    }
    // Characters, not UTF-16 units: 200 emoji is a name (in a project no other test uses).
    expect((await create({ project: 'imported', name: '🧬'.repeat(200) })).status).toBe(201);
    // A project no registered workflow is in.
    const unknown = await create({ project: 'nowhere', name: 'Lost' });
    expect([unknown.status, unknown.text]).toEqual([
      404,
      'unknown project nowhere: no registered workflow is in it',
    ]);
    // An archived one's name is free again.
    const made = await create({ project: 'genomics', name: ' Pilot 2025 ', description: ' ' });
    expect(made.status).toBe(201);
    const exp = obj(made) as unknown as Exp & { description?: string };
    expect([exp.name, exp.run_count, exp.description]).toEqual(['Pilot 2025', 0, undefined]);
    // Newest activity first: just made, it leads the list, ahead of experiments with older runs.
    expect((await listOf())[0].id).toBe(exp.id);

    // Rename, describe, archive and restore.
    const put = (body: unknown) => call('PUT', `/experiments/${q(exp.id)}`, body);
    expect((await put({ name: 'Liver tumor study' })).status).toBe(409);
    expect(obj(await put({ name: 'Pilot 2026', description: 'Round two' }))).toMatchObject({
      name: 'Pilot 2026',
      description: 'Round two',
    });
    expect(obj(await put({ description: null }))).not.toHaveProperty('description');
    expect(obj(await put({ archived: true }))).toHaveProperty('archived_at');
    expect(obj(await put({ archived: false }))).not.toHaveProperty('archived_at');

    // Membership: into an experiment of the run's project, moved, then out.
    const member = (run: string, experiment_id: string | null) =>
      call('POST', `/runs/${q(run)}/experiment`, { experiment_id });
    const RUN = 'run_01HGWEoom';
    expect((await member(RUN, 'exp_umi_tuning')).status).toBe(400);
    expect((await member(RUN, 'nope')).status).toBe(404);
    // The answer is the run's summary after the change.
    expect(obj(await member(RUN, exp.id)).experiment_id).toBe(exp.id);
    expect(obj(await call('GET', `/runs/${RUN}`)).experiment_id).toBe(exp.id);
    expect(
      list<Exp>(await call('GET', '/experiments'), 'experiments').find((e) => e.id === exp.id)
        ?.run_count,
    ).toBe(1);
    // A run older than the experiment leaves its activity at its making: still first.
    expect((await listOf())[0].id).toBe(exp.id);
    expect(obj(await member(RUN, null))).not.toHaveProperty('experiment_id');
    expect(obj(await call('GET', `/runs/${RUN}`))).not.toHaveProperty('experiment_id');
    // Trimmed; blank takes it out too.
    expect(obj(await member(RUN, ` ${exp.id} `)).experiment_id).toBe(exp.id);
    expect(obj(await member(RUN, ''))).not.toHaveProperty('experiment_id');
    // The field is required, even to take it out: a body without it is refused, not read as out.
    for (const body of [{ experiment: exp.id }, { experiment_id: 7 }]) {
      const refused = await call('POST', `/runs/${q(RUN)}/experiment`, body);
      expect([refused.status, refused.text]).toEqual([
        400,
        'the body must name `experiment_id`: an experiment id, or null to take the run out of its experiment',
      ]);
    }

    // Launching into one records it from birth; another project's is refused.
    const launch = { workflow_version_id: 'aabbccddeeff:v1', params: {} };
    expect((await call('POST', '/runs', { ...launch, experiment: 'exp_umi_tuning' })).status).toBe(
      400,
    );
    // At submit an unknown experiment is a field error (400), not a 404.
    expect((await call('POST', '/runs', { ...launch, experiment: 'nope' })).status).toBe(400);
    // Trimmed before it is checked and recorded, as submit trims it.
    const launched = obj(await call('POST', '/runs', { ...launch, experiment: ` ${exp.id} ` }));
    expect(obj(await call('GET', `/runs/${q(String(launched.run_id))}`)).experiment_id).toBe(
      exp.id,
    );
  });

  it('serves FastQC data and alignment views as the QC endpoints do', async () => {
    const DONE = 'run_01HGWDdone';
    const reports = list<{ name: string; sample?: string; output: { id: string } }>(
      await call('GET', `/runs/${DONE}/reports`),
      'reports',
    );
    const of = (name: string, sample: string) =>
      reports.find((r) => r.name === name && r.sample === sample)!.output.id;
    // A report's `declaration` is its index in the `reports:` the run's version serves.
    const { workflow } = obj(await call('GET', `/runs/${DONE}`));
    const form = obj(await call('GET', `/workflows/${q(String(workflow))}`)).form_schema as {
      reports: { name: string }[];
    };
    for (const r of reports as { name: string; declaration?: number }[])
      if (r.declaration != null) expect(form.reports[r.declaration].name, r.name).toBe(r.name);

    // An HTML report's sibling zip, with the report beside it and FastQC's own modules.
    const fastqc = obj(
      await call('GET', `/runs/${DONE}/outputs/${of('Per-sample QC', 'Tumor_1')}/fastqc`),
    );
    expect(fastqc.html_output_id).toBe(of('Per-sample QC', 'Tumor_1'));
    expect(fastqc.version).toBe('0.12.1');
    const statuses = (fastqc.modules as { status: string }[]).map((m) => m.status);
    expect(statuses).toContain('warn');
    expect(statuses).toContain('fail');
    // A zip is its own result; no zip is a 404 (the viewer shows the HTML report).
    expect(
      obj(await call('GET', `/runs/${DONE}/outputs/${fastqc.output_id}/fastqc`)).output_id,
    ).toBe(fastqc.output_id);
    expect(
      (await call('GET', `/runs/${DONE}/outputs/${of('Per-sample QC', 'Normal_2')}/fastqc`)).status,
    ).toBe(404);

    // A BAM with its index, against the run's bundle; a hosted genome by choice; none without one.
    const bam = of('Alignments', 'Tumor_1');
    const view = obj(await call('GET', `/runs/${DONE}/outputs/${bam}/alignment-view`));
    expect(view).toMatchObject({
      locus: 'chrB:9001-11000',
      bucket_cors: 'configured',
      reference: { source: 'bundle', bundle_id: 'grcm39' },
    });
    const hosted = obj(
      await call('GET', `/runs/${DONE}/outputs/${bam}/alignment-view?reference=hg38-igv`),
    );
    expect(hosted.reference).toMatchObject({ source: 'igv_genome', genome: 'hg38' });
    expect(
      (await call('GET', `/runs/${DONE}/outputs/${bam}/alignment-view?reference=nope`)).status,
    ).toBe(404);
    const pca = reports.find((r) => r.name === 'PCA')!.output.id;
    expect((await call('GET', `/runs/${DONE}/outputs/${pca}/alignment-view`)).status).toBe(409);
    const NOREF = 'run_01HGWHnoref';
    const noref = list<{ format: string; output: { id: string } }>(
      await call('GET', `/runs/${NOREF}/reports`),
      'reports',
    ).find((r) => r.format === 'alignment')!;
    const bare = obj(await call('GET', `/runs/${NOREF}/outputs/${noref.output.id}/alignment-view`));
    expect(bare).not.toHaveProperty('reference');
    // Renewing the links only: no locus.
    const renewed = obj(
      await call('GET', `/runs/${DONE}/outputs/${bam}/alignment-view?locus=false`),
    );
    expect(renewed).not.toHaveProperty('locus');
    expect(renewed.reference).toMatchObject({ source: 'bundle', bundle_id: 'grcm39' });
    // Off-CORS links (an imported bucket) say so.
    const off = obj(
      await call('GET', `/runs/${DONE}/outputs/${of('Alignments', 'Normal_2')}/alignment-view`),
    );
    expect(off.bucket_cors).toBe('unknown');
    expect((off.alignment as { url: string }).url).toMatch(/mockCors=off$/);
  });

  it("lists a paired-end sample's files in its one per-sample report, in path order", async () => {
    const reports = list<{ name: string; kind: string; sample?: string; output: { path: string } }>(
      await call('GET', '/runs/run_01HGWHnoref/reports'),
      'reports',
    ).filter((r) => r.kind === 'detected');
    expect(reports.map((r) => [r.sample, r.output.path])).toEqual([
      ['Liver_1', 'fastqc/Liver_1_1_fastqc.zip'],
      ['Liver_1', 'fastqc/Liver_1_2_fastqc.zip'],
      ['Liver_2', 'fastqc/Liver_2_1_fastqc.zip'],
      ['Liver_2', 'fastqc/Liver_2_2_fastqc.zip'],
    ]);
  });

  it("reads a MultiQC-only run's FastQC statuses, each with the sample that owns it", async () => {
    const MQC = 'run_01HGWJmqconly';
    const reports = list<{ kind: string; format: string; output: { id: string } }>(
      await call('GET', `/runs/${MQC}/reports`),
      'reports',
    );
    // No FastQC result of its own to open: only the MultiQC report.
    expect(reports.filter((r) => r.format === 'fastqc')).toEqual([]);
    const multiqc = reports.find((r) => r.kind === 'multiqc')!;
    const status = obj(await call('GET', `/runs/${MQC}/qc/fastqc-status`));
    expect(status.source).toBe('multiqc');
    expect(status.report_output_id).toBe(multiqc.output.id);
    const samples = status.samples as {
      sample?: string;
      fastqc_name: string;
      modules: { name: string; status: string }[];
    }[];
    expect(samples.map((s) => [s.sample, s.fastqc_name])).toEqual([
      ['S1', 'S1_R1'],
      ['S1', 'S1_R2'],
      ['S10', 'S10_R1'],
    ]);
    expect(samples[2].modules[1]).toEqual({ name: 'Per base sequence quality', status: 'fail' });
    expect(samples.every((s) => !('section' in s))).toBe(true);
    // As the backend's `fastqc_statuses` reads the fixture (multiqc.rs's own test).
    expect((samples[2] as unknown as { stats: unknown }).stats).toEqual({
      total_sequences: 1000,
      total_bases: '100 kbp',
      poor_quality: 0,
      sequence_length: '100',
      percent_gc: 30,
      avg_sequence_length: 100,
      median_sequence_length: 100,
      percent_duplicates: 0,
    });
    // A run whose MultiQC report has no FastQC table beside it; a run with no MultiQC report.
    const missing = await call('GET', '/runs/run_01HGWDdone/qc/fastqc-status');
    expect([missing.status, missing.text]).toEqual([
      404,
      'the MultiQC report has no FastQC results',
    ]);
    const noReport = await call('GET', '/runs/run_01HGWHnoref/qc/fastqc-status');
    expect([noReport.status, noReport.text]).toEqual([404, 'this run has no MultiQC report']);
  });

  it('reads every FastQC section of a MultiQC report, raw and trimmed reads apart', async () => {
    const status = obj(await call('GET', '/runs/run_01HGWKmqctrim/qc/fastqc-status'));
    const samples = status.samples as {
      section?: string;
      sample?: string;
      fastqc_name: string;
      stats: { sequence_length?: string };
      modules: { name: string; status: string }[];
    }[];
    // As the backend's `every_fastqc_section_is_read_each_result_tagged_with_its_own`.
    expect(samples.map((s) => [s.section, s.sample, s.fastqc_name])).toEqual([
      ['fastqc_raw', 'S1', 'S1_R1'],
      ['fastqc_raw', 'S1', 'S1_R2'],
      ['fastqc_raw', 'S10', 'S10_R1'],
      ['fastqc_trimmed', 'S1', 'S1_R1'],
      ['fastqc_trimmed', 'S1', 'S1_R2'],
      ['fastqc_trimmed', 'S10', 'S10_R1'],
    ]);
    expect(samples[0].stats.sequence_length).toBe('100');
    expect(samples[3].stats.sequence_length).toBe('80');
    const gc = (i: number) => samples[i].modules.find((m) => m.name === 'Per sequence GC content');
    expect([gc(0)?.status, gc(3)?.status]).toEqual(['warn', 'pass']);
  });

  it('serves the mock files with Range and CORS, or without CORS when asked', async () => {
    const bam = `${origin}/mock-files/igv/tiny.bam`;
    // The backend's own fixture, served as it is.
    const size = statSync(
      fileURLToPath(
        new URL('../../crates/biffix-backend/tests/fixtures/igv/tiny.bam', import.meta.url),
      ),
    ).size;
    const part = await fetch(bam, { headers: { range: 'bytes=0-9' } });
    expect(part.status).toBe(206);
    expect(part.headers.get('content-range')).toBe(`bytes 0-9/${size}`);
    expect((await part.arrayBuffer()).byteLength).toBe(10);
    expect(part.headers.get('access-control-allow-origin')).toBe('*');
    expect(part.headers.get('access-control-expose-headers')).toContain('content-range');
    const tail = await fetch(bam, { headers: { range: 'bytes=-5' } });
    expect(tail.headers.get('content-range')).toBe(`bytes ${size - 5}-${size - 1}/${size}`);
    expect((await fetch(bam, { headers: { range: 'bytes=9999-' } })).status).toBe(416);
    // An invalid range (last before first) is ignored: the whole object, as S3 serves it.
    const backwards = await fetch(bam, { headers: { range: 'bytes=5-2' } });
    expect([backwards.status, (await backwards.arrayBuffer()).byteLength]).toEqual([200, size]);
    const off = await fetch(`${bam}?mockCors=off`, { headers: { range: 'bytes=0-0' } });
    expect(off.headers.get('access-control-allow-origin')).toBeNull();
    expect((await fetch(`${origin}/mock-files/igv/nope`)).status).toBe(404);
    // A link signed for GET refuses HEAD, as S3 does.
    expect((await fetch(bam, { method: 'HEAD' })).status).toBe(403);
  });
});

describe('the mock samples CSV over its limit', () => {
  it('answers a documented 413 that says what to do, not a file cut short', async () => {
    const [small, at] = await startMock({ MOCK_SAMPLES_CSV_MAX_RUNS: '1' });
    try {
      const res = await fetch(`${at}/experiments/exp_liver_tumor/samples.csv`);
      expect(res.status).toBe(413);
      expect(res.headers.get('content-type')).toMatch(/^text\/plain/);
      expect(await res.text()).toBe(
        'This experiment has 2 runs (analyses), more than the 1 one samples download can cover. ' +
          'Split the experiment into smaller ones (move some of its runs into another experiment) ' +
          'and download the samples of each.',
      );
      const op = opFor('GET', '/experiments/exp_liver_tumor/samples.csv');
      expect(op!.op.responses['413'], 'the contract documents the 413').toBeDefined();
      // An experiment within the limit still downloads.
      expect((await fetch(`${at}/experiments/exp_umi_tuning/samples.csv`)).status).toBe(200);
    } finally {
      small.kill();
    }
  });

  it('answers the 413 for too many bytes, counting the rows read so far', async () => {
    // Room for the header and a row: the oldest run's second row is one too many.
    const [small, at] = await startMock({ MOCK_SAMPLES_CSV_MAX_BYTES: '400' });
    try {
      const res = await fetch(`${at}/experiments/exp_liver_tumor/samples.csv`);
      expect(res.status).toBe(413);
      expect(await res.text()).toBe(
        "This experiment's samples are too many to download at once: the file would be larger " +
          'than 400 bytes, the most one download can hold (its oldest 1 of 2 runs (analyses) ' +
          'already give 2 sample rows). Split the experiment into smaller ones (move some of its ' +
          'runs into another experiment) and download the samples of each.',
      );
    } finally {
      small.kill();
    }
  });
});

// Last: every operation the mock routes had a success answer checked above, so a routed operation
// (a new one, or a GET form whose parsing drifts from its POST twin) never escapes the schema check.
describe('the checks above', () => {
  it('checked a success answer of every operation the mock routes, or say why not', async () => {
    const routes = new URL('../mock/routes.mjs', import.meta.url).href;
    const { API_ROUTES } = (await import(/* @vite-ignore */ routes)) as {
      API_ROUTES: [string, string, unknown][];
    };
    const unchecked = API_ROUTES.map(([method, path]) => `${method} ${path}`).filter(
      (op) => !checked.has(op) && !(op in NOT_CHECKED),
    );
    expect(unchecked, 'routed operations no success answer was checked for').toEqual([]);
  });
});
