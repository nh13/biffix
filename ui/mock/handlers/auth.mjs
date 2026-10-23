// The sign-in routes: the deploy's OAuth client (`GET /auth/config`).
// The deploy's OAuth client. Its redirect and sign-out URIs are the UI's own origin: the caller's
// (the dev server on :5173, Playwright's on :5174), or the dev server's when a request has none.
const authConfig = (uiOrigin = 'http://localhost:5173') => ({
  authorization_endpoint: 'https://mock.auth.us-east-1.amazoncognito.com/oauth2/authorize',
  token_endpoint: 'https://mock.auth.us-east-1.amazoncognito.com/oauth2/token',
  client_id: 'mock-client',
  redirect_uri: `${uiOrigin}/auth/callback`,
  logout_endpoint: 'https://mock.auth.us-east-1.amazoncognito.com/logout',
  logout_uri: `${uiOrigin}/signed-out`,
  scopes: ['openid', 'email'],
  user_pool_id: 'us-east-1_mock',
  region: 'us-east-1',
});

/** `GET /auth/config`: the OAuth client, its URIs on the caller's origin. */
function getAuthConfig({ req, send }) {
  send(200, authConfig(req.headers.origin));
}

export const AUTH_ROUTES = [['GET', '/auth/config', getAuthConfig]];
