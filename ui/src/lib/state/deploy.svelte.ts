// Deploy-wide facts the UI needs for display (AWS console deep links, the Cognito pool), read from
// the public `GET /auth/config` once per page load. Shared `$state`, so a link rendered before the
// config arrives fills in when it does.

import { loadOAuthConfig } from '#lib/api/auth.js';

export const deploy = $state<{ region: string | null; userPoolId: string | null }>({
  region: null,
  userPoolId: null,
});

let requested = false;

/**
 * Load {@link deploy} once. Until it loads, or when the deploy does not say, the fields stay null
 * and console links are left out rather than pointed at a guessed region. A failed fetch is retried
 * by the next caller.
 */
export function loadDeploy(): void {
  if (requested) return;
  requested = true;
  loadOAuthConfig()
    .then((config) => {
      deploy.region = config.region ?? null;
      deploy.userPoolId = config.user_pool_id ?? null;
    })
    .catch(() => {
      requested = false;
    });
}
