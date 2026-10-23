import fc from 'fast-check';
import { describe, expect, it, vi } from 'vitest';

import {
  cancelRun,
  getAttempts,
  getRun,
  getTaskLogs,
  listRuns,
  listTasks,
  type RunFilter,
} from './runs';
import {
  jsonResponse,
  requestedBody,
  requestedPath,
  serve,
  TOKEN,
  useSignedInSession,
} from './testing';

// Re-authentication navigates away; the tests only need to know it was started.
vi.mock('./auth', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./auth')>()),
  reauthenticate: vi.fn(async () => true),
}));

useSignedInSession();

describe('runs', () => {
  it('encodes every run id as one path segment', async () => {
    await fc.assert(
      // "." and ".." are the only strings that cannot occupy a URL path segment — the WHATWG URL
      // parser normalizes them away (even percent-encoded as %2e) — and no run id is ever a bare
      // dot-run, so exclude them. Every other string (spaces, any Unicode grapheme incl. astral ones,
      // %, /, …) must survive `encodeURIComponent` as one intact segment.
      fc.asyncProperty(
        fc.string({ unit: 'grapheme' }).filter((id) => id !== '.' && id !== '..'),
        async (runId) => {
          const fetchFn = serve(() => jsonResponse({ run_id: runId, state: 'running', tasks: [] }));
          await getRun(runId);
          expect(requestedPath(fetchFn).endsWith(`/runs/${encodeURIComponent(runId)}`)).toBe(true);
        },
      ),
    );
  });

  it('asks for a run without its tasks only when told to', async () => {
    const fetchFn = serve(() => jsonResponse({ run_id: 'r', state: 'running', tasks: [] }));
    await getRun('r', { tasks: false });
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/r\?tasks=false$/);
  });

  it('sends task page options in a POST body, leaving defaults out and the URL bare', async () => {
    const fetchFn = serve(() => jsonResponse({ tasks: [], total: 0 }));
    const body = (n: number) => requestedBody(fetchFn, n);
    await listTasks('r');
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/r\/tasks\/query$/);
    expect(fetchFn.mock.calls[0][1]!.method).toBe('POST');
    expect(body(0)).toEqual({});
    await listTasks('r', {
      state: 'failed',
      group: 'align',
      q: 'a b',
      sort: 'duration',
      descending: true,
      offset: 200,
      limit: 100,
    });
    // The search text (and a task key searched for) never reaches the request URL.
    expect(requestedPath(fetchFn, 1)).toMatch(/\/runs\/r\/tasks\/query$/);
    expect(body(1)).toEqual({
      state: 'failed',
      group: 'align',
      q: 'a b',
      sort: 'duration',
      order: 'desc',
      offset: 200,
      limit: 100,
    });
  });

  it("sends a task log page's task key and attempt in a POST body, the URL bare", async () => {
    const fetchFn = serve(() => jsonResponse({ events: [] }));
    await getTaskLogs('r', 'align:sample=A', {
      job: 'job-2',
      cursor: 'f/1',
      direction: 'backward',
      limit: 50,
    });
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/r\/task-logs\/query$/);
    expect(fetchFn.mock.calls[0][1]!.method).toBe('POST');
    expect(requestedBody(fetchFn)).toEqual({
      task: 'align:sample=A',
      job: 'job-2',
      cursor: 'f/1',
      direction: 'backward',
      limit: 50,
    });
  });

  it("sends a task's attempts request's task key in a POST body", async () => {
    const fetchFn = serve(() => jsonResponse({ task_key: 'align:sample=A', attempts: [] }));
    await getAttempts('r', 'align:sample=A');
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/r\/attempts\/query$/);
    expect(requestedBody(fetchFn)).toEqual({
      task: 'align:sample=A',
    });
  });

  it('asks for one task by its exact key', async () => {
    const fetchFn = serve(() => jsonResponse({ tasks: [], total: 0 }));
    await listTasks('r', { taskKey: 'sample=P', limit: 1 });
    expect(requestedBody(fetchFn)).toEqual({
      task_key: 'sample=P',
      limit: 1,
    });
  });

  it('serializes every supported server-side filter without changing its value', async () => {
    const filters: RunFilter[] = ['active', 'submitted', 'completed', 'succeeded', 'failed'];
    await fc.assert(
      fc.asyncProperty(fc.constantFrom(...filters), async (filter) => {
        const fetchFn = serve(() => jsonResponse({ runs: [] }));
        await listRuns(filter);
        expect(requestedPath(fetchFn).endsWith('/runs/query')).toBe(true);
        expect(requestedBody(fetchFn)).toEqual({ filter });
      }),
    );
  });

  it('sends the runs search in a POST body, never in the request URL', async () => {
    const fetchFn = serve(() => jsonResponse({ runs: [] }));
    await listRuns('all', { q: 'SAMPLE_X', workflow: 'wf1', limit: 50, before: 'c1' });
    expect(fetchFn.mock.calls[0][1]!.method).toBe('POST');
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/query$/);
    expect(fetchFn.mock.calls[0][0]).not.toContain('SAMPLE_X');
    expect(requestedBody(fetchFn)).toEqual({
      q: 'SAMPLE_X',
      workflow: 'wf1',
      limit: 50,
      before: 'c1',
    });
  });
});

describe('cancelRun', () => {
  it('POSTs to the run cancel route with the token and accepts an empty 202', async () => {
    const fetchFn = serve(() => new Response(null, { status: 202 }));
    await cancelRun('run-1');
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/run-1\/cancel$/);
    expect(fetchFn).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        method: 'POST',
        headers: expect.objectContaining({ authorization: `Bearer ${TOKEN}` }),
      }),
    );
  });

  it('explains a 403 and surfaces any other server error', async () => {
    serve(() => new Response('', { status: 403 }));
    await expect(cancelRun('run-1')).rejects.toThrow("don't have permission to cancel runs");
    serve(() => new Response('terminating the coordinator failed', { status: 502 }));
    await expect(cancelRun('run-1')).rejects.toThrow('terminating the coordinator failed');
    // A run being launched right now: the server's own words, without the status line.
    const launching = 'the run is being launched; retry the cancel in a moment';
    serve(() => new Response(launching, { status: 503 }));
    await expect(cancelRun('run-1')).rejects.toThrow(new RegExp(`^${launching}$`));
    // A bare 503 reads as a hint, not a status line.
    serve(() => new Response('', { status: 503 }));
    await expect(cancelRun('run-1')).rejects.toThrow(
      /^Cancelling is unavailable; retry in a moment$/,
    );
    // Still an ApiError with its status, as every other failure is.
    await expect(cancelRun('run-1')).rejects.toMatchObject({ status: 503 });
  });
});
