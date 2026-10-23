// The overall service health a `GET /status` probe gives: what the sidebar's Status icon shows.

/** `unknown` until the first probe has answered. */
export type Health = 'ok' | 'warn' | 'bad' | 'unknown';

/**
 * The verdict for a probe: its overall status (`null` when /status could not be reached).
 *   ok   = every component healthy
 *   warn = `degraded`: reachable, but a component reports a non-fatal warning
 *   bad  = `down` (a component is down), or /status unreachable
 * A status this UI doesn't know yet (the enum is open) is not claimed healthy: it reads as a warning.
 */
export function healthOfStatus(status: string | null): Exclude<Health, 'unknown'> {
  if (status === null || status === 'down') return 'bad';
  if (status === 'ok') return 'ok';
  return 'warn';
}
