import { describe, expect, it, vi } from 'vitest';

import { ApiError } from './client';
import { getAlignmentView, getOutput, getOutputFastqc, listOutputs } from './outputs';
import { jsonResponse, requestedPath, serve, useSignedInSession } from './testing';

// Re-authentication navigates away; the tests only need to know it was started.
vi.mock('./auth', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./auth')>()),
  reauthenticate: vi.fn(async () => true),
}));

useSignedInSession();

describe('outputs', () => {
  it('sends output page options in a POST body, never in the URL', async () => {
    const fetchFn = serve(() => jsonResponse({ outputs: [], total: 0 }));
    const sent = (n: number) => {
      const init = fetchFn.mock.calls[n][1]!;
      expect(init.method).toBe('POST');
      return JSON.parse(init.body as string);
    };
    await listOutputs('r');
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/r\/outputs\/query$/);
    expect(sent(0)).toEqual({});
    const after = 's3://b/runs/r/results/NA12878.bam';
    await listOutputs('r', { task: 'align:sample=NA12878', roles: ['*'], after, limit: 5 });
    // Neither the task key nor the cursor (an S3 URI) reaches the request URL.
    expect(requestedPath(fetchFn, 1)).toMatch(/\/runs\/r\/outputs\/query$/);
    expect(sent(1)).toEqual({ task: 'align:sample=NA12878', roles: ['*'], after, limit: 5 });
    await listOutputs('r', { roles: ['output', 'log'] });
    expect(sent(2)).toEqual({ roles: ['output', 'log'] });
  });

  it('names one output by id, each id one path segment', async () => {
    const fetchFn = serve(() => jsonResponse({ id: 'a/b' }));
    await getOutput('run 1', 'a/b');
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/run%201\/outputs\/a%2Fb$/);
  });
});

describe('QC previews', () => {
  it("asks for an output's FastQC data, and reads a 404 as none", async () => {
    const fetchFn = serve(() => jsonResponse({ output_id: 'o 1', modules: [] }));
    expect(await getOutputFastqc('run/1', 'o 1')).toEqual({ output_id: 'o 1', modules: [] });
    expect(requestedPath(fetchFn)).toMatch(/\/runs\/run%2F1\/outputs\/o%201\/fastqc$/);
    serve(() => new Response('no FastQC result', { status: 404 }));
    expect(await getOutputFastqc('r', 'o')).toBeNull();
    serve(() => new Response('too large', { status: 422 }));
    await expect(getOutputFastqc('r', 'o')).rejects.toThrow('too large');
  });

  it('asks for an alignment view, against a chosen reference when given', async () => {
    const fetchFn = serve(() => jsonResponse({}));
    await getAlignmentView('r', 'o');
    await getAlignmentView('r', 'o', 'grc m39');
    await getAlignmentView('r', 'o', 'grc m39', { locus: false });
    await getAlignmentView('r', 'o', null, { locus: false });
    expect(requestedPath(fetchFn, 0)).toMatch(/\/runs\/r\/outputs\/o\/alignment-view$/);
    expect(requestedPath(fetchFn, 1)).toMatch(/\/alignment-view\?reference=grc\+m39$/);
    // New links to a file already open skip finding where to open.
    expect(requestedPath(fetchFn, 2)).toMatch(/\/alignment-view\?reference=grc\+m39&locus=false$/);
    expect(requestedPath(fetchFn, 3)).toMatch(/\/alignment-view\?locus=false$/);
  });

  it('keeps the status of a failed response, with the server’s reason', async () => {
    serve(() => new Response('S1.bam has no index', { status: 409 }));
    const failure = getAlignmentView('r', 'o');
    await expect(failure).rejects.toBeInstanceOf(ApiError);
    await expect(failure).rejects.toMatchObject({ status: 409, message: 'S1.bam has no index' });
  });
});
