import { describe, expect, it, vi } from 'vitest';

import { reauthenticate } from './auth';
import { resolveApiBase } from './base';
import { AuthRequiredError } from './client';
import { saveReference } from './references';
import { cancelRun, listRuns, submitRun, type RunSubmitRequest } from './runs';
import { getStatus } from './status';
import {
  jsonResponse,
  requestedPath,
  serve,
  storageWith,
  TOKEN,
  useSignedInSession,
} from './testing';

// Re-authentication navigates away; the tests only need to know it was started.
vi.mock('./auth', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./auth')>()),
  reauthenticate: vi.fn(async () => true),
}));

useSignedInSession();

describe('control-plane API client', () => {
  it('defaults production builds to the same-origin API prefix and sends the access token', async () => {
    expect(resolveApiBase(undefined)).toBe('/api');
    expect(resolveApiBase('https://api.example.test///')).toBe('https://api.example.test');
    const fetchFn = serve(() => jsonResponse({ components: [] }));

    await getStatus();

    expect(requestedPath(fetchFn)).toMatch(/\/status$/);
    expect(fetchFn).toHaveBeenCalledWith(
      expect.stringMatching(/\/status$/),
      expect.objectContaining({
        method: 'GET',
        headers: { accept: 'application/json', authorization: `Bearer ${TOKEN}` },
      }),
    );
  });

  it('reports the response status and request path on failure', async () => {
    serve(() => new Response(null, { status: 503, statusText: 'Unavailable' }));
    await expect(getStatus()).rejects.toThrow('503 Unavailable (GET /status)');
  });

  it("keeps the server's reason, but not a proxy's HTML error page", async () => {
    serve(() => new Response('the run store is unavailable', { status: 502 }));
    await expect(getStatus()).rejects.toThrow(/^the run store is unavailable$/);
    serve(
      () =>
        new Response('<html>Bad gateway</html>', {
          status: 502,
          statusText: 'Bad Gateway',
          headers: { 'content-type': 'text/html' },
        }),
    );
    await expect(getStatus()).rejects.toThrow(/^502 Bad Gateway \(GET \/status\)$/);
  });

  it('sends a JSON body as JSON', async () => {
    const fetchFn = serve(() => jsonResponse({ run_id: 'r' }, { status: 201 }));
    const body = { version: 'v1', params: {} } as unknown as RunSubmitRequest;
    await submitRun(body);
    const init = fetchFn.mock.calls[0][1]!;
    expect(init.method).toBe('POST');
    expect(init.body).toBe(JSON.stringify(body));
    expect(init.headers).toMatchObject({ 'content-type': 'application/json' });
  });

  it('explains a 403 with the permission the call needs', async () => {
    serve(() => new Response('', { status: 403 }));
    await expect(submitRun({} as RunSubmitRequest)).rejects.toThrow(
      /^You don't have permission to launch runs$/,
    );
    await expect(
      saveReference({ id: 'hg38' } as Parameters<typeof saveReference>[0], true),
    ).rejects.toThrow(/^Requires admin access$/);
  });
});

describe('an expired or rejected session', () => {
  it('re-authenticates on a 401 and throws a distinguishable error', async () => {
    serve(() => new Response('token expired', { status: 401 }));
    const failure = getStatus();
    await expect(failure).rejects.toBeInstanceOf(AuthRequiredError);
    expect(reauthenticate).toHaveBeenCalledOnce();
  });

  it('re-authenticates without asking the API when there is no usable token', async () => {
    vi.stubGlobal('sessionStorage', storageWith({ 'biffix-access-token': TOKEN }));
    const fetchFn = serve(() => jsonResponse({}));
    await expect(listRuns('all')).rejects.toBeInstanceOf(AuthRequiredError);
    expect(fetchFn).not.toHaveBeenCalled();
    expect(reauthenticate).toHaveBeenCalledOnce();
  });

  it('says to reload, not that it is signing in, when the loop guard holds the sign-in back', async () => {
    vi.stubGlobal(
      'sessionStorage',
      storageWith({
        'biffix-access-token': TOKEN,
        'biffix-access-token-expiry': String(Date.now() + 3600e3),
        'biffix-reauth-at': String(Date.now()),
      }),
    );
    serve(() => new Response('', { status: 401 }));
    const error = await getStatus().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(AuthRequiredError);
    expect((error as AuthRequiredError).redirecting).toBe(false);
    expect((error as Error).message).toBe(
      'Your session has ended; reload the page to sign in again.',
    );
  });

  it('still throws the auth error when the sign-in config cannot be reached', async () => {
    vi.mocked(reauthenticate).mockRejectedValueOnce(new Error('offline'));
    serve(() => new Response('', { status: 401 }));
    await expect(cancelRun('r')).rejects.toBeInstanceOf(AuthRequiredError);
  });
});
