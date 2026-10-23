import { API_BASE } from './base';
import type { components } from '#lib/generated/api.js';
import { base64UrlDecode, base64UrlEncode } from '#lib/util/base64url.js';

/** `GET /auth/config`: the deploy's public OAuth client configuration (generated from the API). */
export type OAuthClientConfig = components['schemas']['OAuthClientConfig'];

/** The token endpoint's response, as received: every field is checked before it is trusted. */
interface TokenResponse {
  access_token?: unknown;
  id_token?: unknown;
  expires_in?: unknown;
  token_type?: unknown;
}

const ACCESS_TOKEN = 'biffix-access-token';
const ACCESS_TOKEN_EXPIRY = 'biffix-access-token-expiry';
const ID_TOKEN = 'biffix-id-token';
const VERIFIER = 'biffix-pkce-verifier';
const OAUTH_STATE = 'biffix-oauth-state';
const RETURN_TO = 'biffix-return-to';
const REAUTH_AT = 'biffix-reauth-at';

/**
 * `value` when it is a same-origin path, else `/`. A single leading `/` is required, and it must
 * not be followed by `/` or a backslash: browsers read `//host` and `/\host` as protocol-relative,
 * i.e. an off-site target. Applied both when the target is stored and when it is read back.
 */
export function safeReturnTo(value: string | null | undefined): string {
  return value && /^\/(?![/\\])/.test(value) ? value : '/';
}

export async function pkceChallenge(verifier: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier));
  return base64UrlEncode(new Uint8Array(digest));
}

function randomValue(bytes = 32): string {
  const value = new Uint8Array(bytes);
  crypto.getRandomValues(value);
  return base64UrlEncode(value);
}

export function buildAuthorizationUrl(
  config: OAuthClientConfig,
  challenge: string,
  state: string,
): string {
  const url = new URL(config.authorization_endpoint);
  url.search = new URLSearchParams({
    response_type: 'code',
    client_id: config.client_id,
    redirect_uri: config.redirect_uri,
    scope: config.scopes.join(' '),
    code_challenge_method: 'S256',
    code_challenge: challenge,
    state,
  }).toString();
  return url.toString();
}

// Validate the discovery response at the trust boundary. The endpoint is IdP-/backend-controlled
// and the body is otherwise cast blind, so without this a missing or mistyped field surfaces far
// downstream as a raw `TypeError` (`new URL(undefined)`, `undefined.join(...)`, `undefined
// .toLowerCase()`) instead of one clean, actionable error here. Every consumer (buildAuthorizationUrl,
// buildLogoutUrl, completeLogin) can then trust the shape.
function validateOAuthConfig(raw: unknown): OAuthClientConfig {
  const c = raw as Record<string, unknown> | null;
  if (!c || typeof c !== 'object')
    throw new Error('OAuth configuration is malformed (not an object)');
  const requiredStrings = [
    'authorization_endpoint',
    'token_endpoint',
    'client_id',
    'redirect_uri',
    'logout_endpoint',
    'logout_uri',
  ] as const;
  for (const key of requiredStrings) {
    if (typeof c[key] !== 'string' || !(c[key] as string)) {
      throw new Error(`OAuth configuration is malformed (missing "${key}")`);
    }
  }
  if (!Array.isArray(c.scopes) || c.scopes.some((s) => typeof s !== 'string')) {
    throw new Error('OAuth configuration is malformed (missing "scopes")');
  }
  return c as unknown as OAuthClientConfig;
}

export async function loadOAuthConfig(fetchFn: typeof fetch = fetch): Promise<OAuthClientConfig> {
  // Same base resolution as the data client, so the documented `PUBLIC_BIFFIX_API` dev override
  // covers auth discovery too (not just `/runs`, `/status`, …).
  const response = await fetchFn(`${API_BASE}/auth/config`, {
    headers: { accept: 'application/json' },
  });
  if (!response.ok) throw new Error(`OAuth configuration unavailable (${response.status})`);
  return validateOAuthConfig(await response.json());
}

/** The stored access token, unless there is none or it expires within 30 s. */
export function getAccessToken(storage: Storage = sessionStorage): string | null {
  const token = storage.getItem(ACCESS_TOKEN);
  const expiry = Number(storage.getItem(ACCESS_TOKEN_EXPIRY) ?? 0);
  if (!token || !expiry || Date.now() >= expiry - 30_000) return null;
  return token;
}

/** Start a sign-in: store a PKCE verifier, a state and where to return; resolves to the IdP URL. */
export async function beginLogin(
  config: OAuthClientConfig,
  returnTo: string,
  storage: Storage = sessionStorage,
): Promise<string> {
  const verifier = randomValue(64);
  const state = randomValue();
  storage.setItem(VERIFIER, verifier);
  storage.setItem(OAUTH_STATE, state);
  storage.setItem(RETURN_TO, safeReturnTo(returnTo));
  return buildAuthorizationUrl(config, await pkceChallenge(verifier), state);
}

/** Go to the IdP to sign in (back to the current location) unless `storage` holds a usable token. */
export async function ensureAuthenticated(storage: Storage = sessionStorage): Promise<boolean> {
  if (getAccessToken(storage)) return true;
  const config = await loadOAuthConfig();
  const destination = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  window.location.assign(await beginLogin(config, destination, storage));
  return false;
}

/** How long after one forced re-authentication another is refused (the redirect-loop guard). */
export const REAUTH_COOLDOWN_MS = 60_000;

/**
 * Claim the right to force a re-authentication now, recording it. False when one was claimed within
 * {@link REAUTH_COOLDOWN_MS}: an API that rejects even a freshly issued token (a misconfigured
 * audience, a clock far off) must not bounce the browser through the IdP forever, and a burst of
 * failing requests (every live view's poll at once) must redirect once, not once each.
 */
export function claimReauth(storage: Storage = sessionStorage, now = Date.now()): boolean {
  if (reauthHeld(storage, now)) return false;
  storage.setItem(REAUTH_AT, String(now));
  return true;
}

/** Whether {@link claimReauth} would refuse now: a forced re-authentication was claimed lately. */
export function reauthHeld(storage: Storage = sessionStorage, now = Date.now()): boolean {
  const last = Number(storage.getItem(REAUTH_AT) ?? 0);
  return Number.isFinite(last) && last > 0 && now - last < REAUTH_COOLDOWN_MS;
}

/**
 * Sign in again after the API refused the session (no usable token, or a 401), returning to the
 * current location. The stored token is dropped first, else {@link ensureAuthenticated} would see
 * a token the server has already rejected and do nothing. The IdP session usually survives, so
 * this is a silent round trip. Resolves true when it started a redirect, false when the loop guard
 * ({@link claimReauth}) held it back.
 */
export async function reauthenticate(storage: Storage = sessionStorage): Promise<boolean> {
  if (!claimReauth(storage)) return false;
  await signInAgain(storage);
  return true;
}

/**
 * Drop the stored token and sign in again, back to the current location. Not held back by the
 * loop guard: it is for a person who asked to (the signed-out page's *Sign in*).
 */
export async function signInAgain(storage: Storage = sessionStorage): Promise<void> {
  storage.removeItem(ACCESS_TOKEN);
  storage.removeItem(ACCESS_TOKEN_EXPIRY);
  await ensureAuthenticated(storage);
}

/**
 * Finish a sign-in on the IdP's callback: check its state against the one this tab stored, trade the
 * code for tokens (PKCE) and store them; resolves to the page to return to.
 */
export async function completeLogin(
  callbackUrl: URL,
  fetchFn: typeof fetch = fetch,
  storage: Storage = sessionStorage,
): Promise<string> {
  const error = callbackUrl.searchParams.get('error');
  if (error) throw new Error(callbackUrl.searchParams.get('error_description') ?? error);
  const code = callbackUrl.searchParams.get('code');
  const state = callbackUrl.searchParams.get('state');
  const expectedState = storage.getItem(OAUTH_STATE);
  const verifier = storage.getItem(VERIFIER);
  // Each says what is wrong; a sign-in started afresh (Try again) fixes every one.
  const again = 'start signing in again';
  if (!code) throw new Error(`The sign-in answer has no code; ${again}`);
  if (!state || !expectedState || state !== expectedState)
    throw new Error(
      `The sign-in answer does not match a sign-in this tab started (state); ${again}`,
    );
  if (!verifier) throw new Error(`This tab lost its sign-in check (PKCE verifier); ${again}`);

  const config = await loadOAuthConfig(fetchFn);
  const body = new URLSearchParams({
    grant_type: 'authorization_code',
    client_id: config.client_id,
    redirect_uri: config.redirect_uri,
    code_verifier: verifier,
    code,
  });
  const response = await fetchFn(config.token_endpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body,
  });
  if (!response.ok) throw new Error(`OAuth token exchange failed (${response.status})`);
  const tokens = ((await response.json()) ?? {}) as TokenResponse;
  // The body is unvalidated, so check each field before storing anything: an out-of-spec response
  // must yield one clean error, not a raw TypeError.
  if (
    typeof tokens.access_token !== 'string' ||
    !tokens.access_token ||
    typeof tokens.token_type !== 'string' ||
    tokens.token_type.toLowerCase() !== 'bearer'
  ) {
    throw new Error('OAuth token response did not contain a bearer access token');
  }
  // Without a usable lifetime the stored expiry would be NaN, getAccessToken would report no
  // token, and every page load would bounce through the IdP and back, forever.
  if (
    typeof tokens.expires_in !== 'number' ||
    !Number.isFinite(tokens.expires_in) ||
    tokens.expires_in <= 0
  ) {
    throw new Error('OAuth token response did not contain a valid expires_in');
  }
  storage.setItem(ACCESS_TOKEN, tokens.access_token);
  storage.setItem(ACCESS_TOKEN_EXPIRY, String(Date.now() + tokens.expires_in * 1000));
  if (typeof tokens.id_token === 'string' && tokens.id_token)
    storage.setItem(ID_TOKEN, tokens.id_token);
  const returnTo = safeReturnTo(storage.getItem(RETURN_TO));
  storage.removeItem(VERIFIER);
  storage.removeItem(OAUTH_STATE);
  storage.removeItem(RETURN_TO);
  return returnTo;
}

/**
 * The signed-in user's ID-token claims, or `null` without a readable ID token.
 *
 * The ID token is NOT used for authorization anywhere (the API accepts only the access token), so
 * decoding it client-side to shape what the page shows is safe: a tampered ID token can change
 * what the page says and nothing else.
 */
function idClaims(storage: Storage): Record<string, unknown> | null {
  const token = storage.getItem(ID_TOKEN);
  if (!token) return null;
  const payload = token.split('.')[1];
  if (!payload) return null;
  try {
    // base64url → bytes → UTF-8: `atob` alone yields one Latin-1 char per byte, garbling any
    // non-ASCII email or username.
    const json = new TextDecoder().decode(base64UrlDecode(payload));
    const claims: unknown = JSON.parse(json);
    return claims && typeof claims === 'object' ? (claims as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

const text = (v: unknown): string | null => (typeof v === 'string' && v ? v : null);

/** The signed-in user's email, for display only (see `idClaims`). */
export function currentIdentity(storage: Storage = sessionStorage): string | null {
  const claims = idClaims(storage);
  return claims ? (text(claims.email) ?? text(claims['cognito:username'])) : null;
}

/**
 * The signed-in user's id (the `sub` the server records as `created_by`), for presentation only
 * (see `idClaims`): whether a hint is meant for them, never whether they may do something.
 */
export function currentSubject(storage: Storage = sessionStorage): string | null {
  return text(idClaims(storage)?.sub);
}

/**
 * Clear this app's session. Exported separately so sign-out still works when the IdP is
 * unreachable: local credentials must always be removable.
 */
export function clearSession(storage: Storage = sessionStorage): void {
  for (const key of [
    ACCESS_TOKEN,
    ACCESS_TOKEN_EXPIRY,
    ID_TOKEN,
    VERIFIER,
    OAUTH_STATE,
    RETURN_TO,
    REAUTH_AT,
  ]) {
    storage.removeItem(key);
  }
}

/**
 * Build the Cognito logout URL, having cleared local credentials first.
 *
 * Order matters: clear locally BEFORE navigating, so a user who closes the tab mid-redirect is
 * still signed out here. Ending the Cognito session is what stops the app from silently
 * re-authenticating on the next page load; without it "sign out" only reshuffles local storage.
 *
 * Note this does NOT sign the user out of Google itself — Cognito cannot end an upstream IdP
 * session. Signing back in may therefore not re-prompt for Google credentials, which is the
 * expected behaviour of federated logout, not a bug.
 */
export function buildLogoutUrl(
  config: OAuthClientConfig,
  storage: Storage = sessionStorage,
): string {
  clearSession(storage);
  const url = new URL(config.logout_endpoint);
  url.search = new URLSearchParams({
    client_id: config.client_id,
    logout_uri: config.logout_uri,
  }).toString();
  return url.toString();
}

/** Sign out: end the local session and the Cognito session. */
export async function logout(fetchFn: typeof fetch = fetch): Promise<void> {
  let target: string;
  try {
    target = buildLogoutUrl(await loadOAuthConfig(fetchFn));
  } catch {
    // The IdP session may survive, but the local one must not: fall back to the signed-out page.
    clearSession();
    target = '/signed-out';
  }
  window.location.assign(target);
}
