// Where the control-plane API lives. Production serves the UI and API same-origin under `/api`
// (through CloudFront); in local dev they may run on different origins, so `PUBLIC_BIFFIX_API`
// points the UI at the backend. This is shared by BOTH the typed API client (`client.ts`) AND the
// OAuth discovery call (`auth.ts`) so the two can never disagree about where the backend is — a
// hardcoded `/api/auth/config` in one of them would 404 in exactly the cross-origin dev setup the
// override exists for.

/** Normalize a configured base (strip trailing slashes); fall back to the same-origin `/api`. */
export function resolveApiBase(configuredBase?: string): string {
  return configuredBase?.replace(/\/+$/, '') || '/api';
}

export const API_BASE = resolveApiBase(
  import.meta.env.DEV ? import.meta.env.PUBLIC_BIFFIX_API : undefined,
);
