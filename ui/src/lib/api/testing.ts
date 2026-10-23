// What the API client's tests share: a signed-in session and a stubbed `fetch`. A test file that
// uses `useSignedInSession` must also mock `./auth`'s `reauthenticate` (re-authentication navigates
// away; the tests only need to know it was started), since `vi.mock` applies per file:
//
//   vi.mock('./auth', async (importOriginal) => ({
//     ...(await importOriginal<typeof import('./auth')>()),
//     reauthenticate: vi.fn(async () => true),
//   }));

import { afterEach, beforeEach, vi } from 'vitest';

import { reauthenticate } from './auth';

export const TOKEN = 'cognito-access-token';

/** A session-storage stand-in (the tests run under node, which has none). */
export function storageWith(entries: Record<string, string>): Storage {
  const values = new Map(Object.entries(entries));
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (k) => values.get(k) ?? null,
    key: (i) => [...values.keys()][i] ?? null,
    removeItem: (k) => void values.delete(k),
    setItem: (k, v) => void values.set(k, v),
  };
}

/** Sign in: a live access token (and its expiry — a token without one counts as none). */
function signIn(token = TOKEN) {
  vi.stubGlobal(
    'sessionStorage',
    storageWith({
      'biffix-access-token': token,
      'biffix-access-token-expiry': String(Date.now() + 3600e3),
    }),
  );
}

/** Sign in before each test of the calling file; undo the stubs after each. */
export function useSignedInSession() {
  beforeEach(() => signIn());
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.mocked(reauthenticate).mockClear();
  });
}

/** Answer every request with `respond`, returning the spy. */
export function serve(respond: () => Response) {
  const fetchFn = vi.fn(async (_input: string, _init?: RequestInit) => respond());
  vi.stubGlobal('fetch', fetchFn);
  return fetchFn;
}

export function jsonResponse(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}

/** The path and query of the `n`th request. */
export function requestedPath(fetchFn: ReturnType<typeof serve>, n = 0): string {
  const url = new URL(fetchFn.mock.calls[n][0], 'https://ui.example.test');
  return url.pathname + url.search;
}

/** The JSON body of the `n`th request. */
export function requestedBody(fetchFn: ReturnType<typeof serve>, n = 0): unknown {
  return JSON.parse(String(fetchFn.mock.calls[n][1]?.body));
}
