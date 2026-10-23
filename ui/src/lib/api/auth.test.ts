import { afterEach, describe, expect, it, vi } from 'vitest';

import { base64UrlEncode } from '$lib/util/base64url';

import {
  beginLogin,
  buildAuthorizationUrl,
  buildLogoutUrl,
  claimReauth,
  clearSession,
  completeLogin,
  currentIdentity,
  currentSubject,
  getAccessToken,
  loadOAuthConfig,
  logout,
  pkceChallenge,
  reauthenticate,
  signInAgain,
  REAUTH_COOLDOWN_MS,
  safeReturnTo,
  type OAuthClientConfig,
} from './auth';
import { storageWith } from './testing';

const config: OAuthClientConfig = {
  authorization_endpoint: 'https://tenant.auth.us-east-1.amazoncognito.com/oauth2/authorize',
  token_endpoint: 'https://tenant.auth.us-east-1.amazoncognito.com/oauth2/token',
  client_id: 'client-123',
  redirect_uri: 'https://app.example/auth/callback',
  logout_endpoint: 'https://tenant.auth.us-east-1.amazoncognito.com/logout',
  logout_uri: 'https://app.example',
  scopes: ['openid', 'email'],
};

describe('Cognito authorization-code PKCE flow', () => {
  it('constructs an S256 authorization request without a client secret', () => {
    const url = new URL(buildAuthorizationUrl(config, 'challenge', 'state'));
    expect(url.searchParams.get('response_type')).toBe('code');
    expect(url.searchParams.get('client_id')).toBe(config.client_id);
    expect(url.searchParams.get('code_challenge_method')).toBe('S256');
    expect(url.searchParams.get('code_challenge')).toBe('challenge');
    expect(url.searchParams.has('client_secret')).toBe(false);
  });

  it('exchanges a matching callback and persists the access token', async () => {
    const storage = storageWith({});
    const callback = await callbackFor(storage);
    const fetchFn = tokenServer(BEARER);

    await expect(completeLogin(callback, fetchFn as typeof fetch, storage)).resolves.toBe(
      '/runs/r1',
    );
    expect(getAccessToken(storage)).toBe('access-token');
  });

  it('rejects a callback whose state does not match (CSRF guard)', async () => {
    const storage = storageWith({});
    await beginLogin(config, '/runs/r1', storage); // stores the expected state + verifier
    const callback = new URL(config.redirect_uri);
    callback.searchParams.set('code', 'authorization-code');
    callback.searchParams.set('state', 'forged-state'); // ≠ the stored one
    const fetchFn = vi.fn(); // must never be reached — the exchange is refused before any network
    await expect(completeLogin(callback, fetchFn as typeof fetch, storage)).rejects.toThrow(
      /does not match a sign-in this tab started \(state\); start signing in again$/,
    );
    expect(fetchFn).not.toHaveBeenCalled();
    expect(getAccessToken(storage)).toBeNull();
  });

  it('surfaces an IdP error redirect with its description', async () => {
    const callback = new URL(config.redirect_uri);
    callback.searchParams.set('error', 'access_denied');
    callback.searchParams.set('error_description', 'user cancelled at Google');
    await expect(
      completeLogin(callback, vi.fn() as unknown as typeof fetch, storageWith({})),
    ).rejects.toThrow('user cancelled at Google');
  });

  it('rejects a token response that is not a bearer token', async () => {
    const storage = storageWith({});
    const callback = await callbackFor(storage);
    // access_token present but token_type omitted — an out-of-spec response must yield the clean
    // error, not a raw "Cannot read properties of undefined (reading 'toLowerCase')".
    const fetchFn = tokenServer({ access_token: 'x', expires_in: 3600 });
    await expect(completeLogin(callback, fetchFn as typeof fetch, storage)).rejects.toThrow(
      /did not contain a bearer access token/,
    );
    expect(getAccessToken(storage)).toBeNull();
  });
});

describe('sign out', () => {
  function signedIn(): Storage {
    return storageWith({
      'biffix-access-token': 'access-token',
      'biffix-access-token-expiry': String(Date.now() + 60_000),
      // An ID token whose payload carries the email; only the payload segment matters here.
      'biffix-id-token': idToken({ email: 'alice@lab.example.org' }),
    });
  }

  it('clears every credential and login leftover it holds', () => {
    const storage = signedIn();
    storage.setItem('biffix-pkce-verifier', 'v');
    storage.setItem('biffix-oauth-state', 's');
    storage.setItem('biffix-return-to', '/runs');
    storage.setItem('biffix-reauth-at', '1');
    clearSession(storage);
    expect(storage.length).toBe(0);
  });

  it('ends the Cognito session, not just the local one', () => {
    // A sign-out that only clears local storage leaves the IdP session alive and the app is
    // silently signed back in on the next load. The logout URL is what actually ends it.
    const url = new URL(buildLogoutUrl(config, signedIn()));
    expect(url.origin + url.pathname).toBe(config.logout_endpoint);
    expect(url.searchParams.get('client_id')).toBe(config.client_id);
    expect(url.searchParams.get('logout_uri')).toBe(config.logout_uri);
  });

  it('clears local credentials before redirecting', () => {
    // If the user closes the tab mid-redirect they must still be signed out locally.
    const storage = signedIn();
    buildLogoutUrl(config, storage);
    expect(storage.getItem('biffix-access-token')).toBeNull();
  });

  it('signs out locally, to /signed-out, when the sign-in config cannot be read', async () => {
    const storage = signedIn();
    const assign = vi.fn();
    vi.stubGlobal('sessionStorage', storage);
    vi.stubGlobal('window', { location: { assign } });
    try {
      await logout(async () => new Response('', { status: 503 }));
      expect(assign).toHaveBeenCalledWith('/signed-out');
      expect(storage.getItem('biffix-access-token')).toBeNull();
      expect(storage.getItem('biffix-id-token')).toBeNull();
      // A reachable config ends the IdP session too.
      assign.mockClear();
      await logout(async () => new Response(JSON.stringify(config), { status: 200 }));
      expect(String(assign.mock.calls[0][0])).toMatch(new RegExp(`^${config.logout_endpoint}`));
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('reads the display identity from the ID token, and tolerates a missing or broken one', () => {
    expect(currentIdentity(signedIn())).toBe('alice@lab.example.org');
    expect(currentIdentity(storageWith({}))).toBeNull();
    const broken = storageWith({ 'biffix-id-token': 'not-a-jwt' });
    expect(currentIdentity(broken)).toBeNull();
  });

  it('decodes a base64url ID-token payload as UTF-8', () => {
    // Non-ASCII, and long enough that the unpadded base64url payload carries `-` or `_`.
    const claims = { email: 'zoë.ñúñez@exämple.org', 'cognito:username': '>>>???~~~' };
    const storage = storageWith({ 'biffix-id-token': idToken(claims) });
    expect(storage.getItem('biffix-id-token')!.split('.')[1]).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(storage.getItem('biffix-id-token')!.split('.')[1]).toMatch(/[-_]/);
    expect(currentIdentity(storage)).toBe(claims.email);
    const username = storageWith({ 'biffix-id-token': idToken({ 'cognito:username': 'jürgen' }) });
    expect(currentIdentity(username)).toBe('jürgen');
  });

  it("reads the user's id (`sub`) from the ID token, and tolerates a missing one", () => {
    const storage = storageWith({
      'biffix-id-token': idToken({ sub: 'user-1', email: 'a@example.org' }),
    });
    expect(currentSubject(storage)).toBe('user-1');
    const noSub = storageWith({ 'biffix-id-token': idToken({ email: 'a@example.org' }) });
    expect(currentSubject(noSub)).toBeNull();
    expect(currentSubject(storageWith({}))).toBeNull();
  });
});

/** A JWT-shaped token whose payload is `claims`, base64url-encoded (UTF-8, no padding). */
function idToken(claims: object): string {
  return `header.${base64UrlEncode(new TextEncoder().encode(JSON.stringify(claims)))}.signature`;
}

/** A token endpoint (and /auth/config) answering with `tokens`, recording each request. */
function tokenServer(tokens: object) {
  return vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) =>
    String(input).endsWith('/auth/config')
      ? new Response(JSON.stringify(config), { status: 200 })
      : new Response(JSON.stringify(tokens), { status: 200 }),
  );
}

/** A callback URL for the login `beginLogin` just started in `storage`. */
async function callbackFor(storage: Storage, returnTo = '/runs/r1'): Promise<URL> {
  const authorization = new URL(await beginLogin(config, returnTo, storage));
  const callback = new URL(config.redirect_uri);
  callback.searchParams.set('code', 'authorization-code');
  callback.searchParams.set('state', authorization.searchParams.get('state')!);
  return callback;
}

const BEARER = { access_token: 'access-token', expires_in: 3600, token_type: 'Bearer' };

describe('the PKCE and session contract', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('derives the S256 challenge of RFC 7636 appendix B', async () => {
    await expect(pkceChallenge('dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk')).resolves.toBe(
      'E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM',
    );
  });

  it('starts a login with URL-safe state and challenge, and the stored verifier behind it', async () => {
    const storage = storageWith({});
    const url = new URL(await beginLogin(config, '/runs', storage));
    const urlSafe = /^[A-Za-z0-9_-]+$/;
    expect(url.searchParams.get('state')).toMatch(urlSafe);
    expect(url.searchParams.get('state')).toBe(storage.getItem('biffix-oauth-state'));
    expect(url.searchParams.get('code_challenge')).toMatch(urlSafe);
    const verifier = storage.getItem('biffix-pkce-verifier')!;
    expect(verifier).toMatch(urlSafe);
    await expect(pkceChallenge(verifier)).resolves.toBe(url.searchParams.get('code_challenge'));
  });

  it('exchanges the code with the stored verifier and clears the login leftovers', async () => {
    const storage = storageWith({});
    const callback = await callbackFor(storage);
    const verifier = storage.getItem('biffix-pkce-verifier');
    const fetchFn = tokenServer(BEARER);
    await completeLogin(callback, fetchFn as typeof fetch, storage);
    const [url, init] = fetchFn.mock.calls[1];
    expect(String(url)).toBe(config.token_endpoint);
    const body = new URLSearchParams(String(init?.body));
    expect(body.get('grant_type')).toBe('authorization_code');
    expect(body.get('code')).toBe('authorization-code');
    expect(body.get('code_verifier')).toBe(verifier);
    expect(body.get('redirect_uri')).toBe(config.redirect_uri);
    expect(body.get('client_id')).toBe(config.client_id);
    for (const key of ['biffix-pkce-verifier', 'biffix-oauth-state', 'biffix-return-to'])
      expect(storage.getItem(key)).toBeNull();
  });

  it('refuses a token response without a usable expires_in, storing nothing', async () => {
    for (const expires_in of [undefined, 'soon', 0, -5]) {
      const storage = storageWith({});
      const callback = await callbackFor(storage);
      await expect(
        completeLogin(callback, tokenServer({ ...BEARER, expires_in }) as typeof fetch, storage),
      ).rejects.toThrow(/valid expires_in/);
      expect(storage.getItem('biffix-access-token')).toBeNull();
      expect(storage.getItem('biffix-access-token-expiry')).toBeNull();
    }
  });

  it('treats a token as gone from 30 s before it expires', () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    const storage = storageWith({});
    storage.setItem('biffix-access-token', 't');
    storage.setItem('biffix-access-token-expiry', String(1_000_000 + 31_000));
    expect(getAccessToken(storage)).toBe('t');
    vi.setSystemTime(1_000_000 + 1_000);
    expect(getAccessToken(storage)).toBeNull();
    storage.setItem('biffix-access-token-expiry', String(1_000_000 - 1));
    expect(getAccessToken(storage)).toBeNull();
  });

  it('keeps a return target on this site, on the way in and on the way back', async () => {
    for (const offsite of [
      'https://evil.example',
      'evil',
      '//evil.example/x',
      '/\\evil.example',
      '',
    ]) {
      const storage = storageWith({});
      await beginLogin(config, offsite, storage);
      expect(storage.getItem('biffix-return-to')).toBe('/');
    }
    expect(safeReturnTo('/runs/r1?tab=jobs#x')).toBe('/runs/r1?tab=jobs#x');
    // A tampered stored value is sanitized when read back too.
    const storage = storageWith({});
    const callback = await callbackFor(storage);
    storage.setItem('biffix-return-to', '//evil.example/x');
    await expect(
      completeLogin(callback, tokenServer(BEARER) as typeof fetch, storage),
    ).resolves.toBe('/');
  });

  it('names the missing field of a malformed OAuth configuration', async () => {
    const serve = (body: object) =>
      vi.fn(async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch;
    const { client_id: _omit, ...noClient } = config;
    void _omit;
    await expect(loadOAuthConfig(serve(noClient))).rejects.toThrow(
      'OAuth configuration is malformed (missing "client_id")',
    );
    await expect(loadOAuthConfig(serve({ ...config, scopes: 'openid' }))).rejects.toThrow(
      'OAuth configuration is malformed (missing "scopes")',
    );
  });

  it('forces at most one re-authentication per cooldown (the redirect-loop guard)', () => {
    const storage = storageWith({});
    expect(claimReauth(storage, 1_000)).toBe(true);
    expect(claimReauth(storage, 1_000 + REAUTH_COOLDOWN_MS - 1)).toBe(false);
    expect(claimReauth(storage, 1_000 + REAUTH_COOLDOWN_MS)).toBe(true);
  });

  it('re-authenticates by dropping the rejected token and signing in back to here', async () => {
    const storage = storageWith({
      'biffix-access-token': 'rejected',
      'biffix-access-token-expiry': String(Date.now() + 3600e3),
    });
    const assign = vi.fn();
    vi.stubGlobal('sessionStorage', storage);
    vi.stubGlobal('window', {
      location: { pathname: '/runs/r1', search: '?tab=jobs', hash: '', assign },
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(config), { status: 200 })),
    );
    await expect(reauthenticate(storage)).resolves.toBe(true);
    expect(storage.getItem('biffix-access-token')).toBeNull();
    expect(assign).toHaveBeenCalledOnce();
    expect(String(assign.mock.calls[0][0])).toMatch(/^https:\/\/tenant\.auth\./);
    expect(storage.getItem('biffix-return-to')).toBe('/runs/r1?tab=jobs');
    // A second rejection inside the cooldown does not bounce the browser again.
    await expect(reauthenticate(storage)).resolves.toBe(false);
    expect(assign).toHaveBeenCalledOnce();
    // A person who asks to sign in is never held back by it.
    storage.setItem('biffix-access-token', 'rejected');
    storage.setItem('biffix-access-token-expiry', String(Date.now() + 3600e3));
    await signInAgain(storage);
    expect(assign).toHaveBeenCalledTimes(2);
  });

  it('signs in again through the storage it is given', async () => {
    const storage = storageWith({
      'biffix-access-token': 'rejected',
      'biffix-access-token-expiry': String(Date.now() + 3600e3),
    });
    // The tab's own session storage still holds a live token: it must not be what is read.
    const tab = storageWith({
      'biffix-access-token': 'live',
      'biffix-access-token-expiry': String(Date.now() + 3600e3),
    });
    const assign = vi.fn();
    vi.stubGlobal('sessionStorage', tab);
    vi.stubGlobal('window', { location: { pathname: '/', search: '', hash: '', assign } });
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(config), { status: 200 })),
    );
    await expect(reauthenticate(storage)).resolves.toBe(true);
    expect(assign).toHaveBeenCalledOnce();
    expect(storage.getItem('biffix-oauth-state')).not.toBeNull();
    expect(tab.getItem('biffix-oauth-state')).toBeNull();
  });
});
