import type { TaskAttempt, Host } from '#lib/api/index.js';
import { fmtBytes, fmtWallTime } from '#lib/util/format.js';
import { jobCostText } from '#lib/runs/cost.js';

/**
 * Why an attempt ended, as one lowercase cause ranked the way the server's failure diagnosis ranks
 * it (`biffix_core::failure::diagnose`): out of memory first, then a spot reclaim (EC2 warned that
 * its spot host would be reclaimed, or Batch says `spot`), then Batch's own category. A try warned
 * of a reclaim may still have run out of memory first; a host Batch saw terminate after a warning
 * was reclaimed. The Expert view's `terminationLabel` and the Lab view's try lines both word this
 * one answer, so they never disagree. Null when nothing is known.
 */
export function attemptEnding(t: {
  spot_warned_at?: number | null;
  termination_category?: string | null;
}): string | null {
  const category = t.termination_category?.toLowerCase() || null;
  if (category === 'oom') return 'oom';
  if (t.spot_warned_at != null || category === 'spot') return 'spot';
  return category;
}

/**
 * Why an attempt ended, for display (`attemptEnding`): `spot reclaim` for a reclaimed spot host,
 * `no result reported (run ended)` for a task left open when its run ended (shown failed, though it
 * never reported a failure), else Batch's category in words (`host terminated`).
 */
export function terminationLabel(t: {
  spot_warned_at?: number | null;
  termination_category?: string | null;
}): string | undefined {
  const ending = attemptEnding(t);
  if (ending === 'spot') return 'spot reclaim';
  if (ending === 'run_ended') return 'no result reported (run ended)';
  return ending?.replaceAll('_', ' ');
}

/**
 * Why the workflow engine failed a task whose AWS Batch job did not fail (e.g. it did not write its
 * declared outputs), for display; undefined when it did not.
 */
export function verdictLabel(t: { verdict_reason?: string | null }): string | undefined {
  const reason = t.verdict_reason?.trim();
  return reason ? `Failed by the workflow engine: ${reason}` : undefined;
}

/** An EC2 lifecycle for display: `spot`, `on-demand`, `capacity-block`… */
export function lifecycleLabel(lifecycle?: string | null): string | undefined {
  return lifecycle?.replaceAll('_', '-');
}

/** A host's capacity for display (`lifecycleLabel`). */
export function capacityLabel(host?: Host | null): string | undefined {
  return lifecycleLabel(host?.capacity);
}

/** A host's type and AZ, e.g. `c8g.16xlarge us-east-1b`; undefined when neither is known. */
export function hostLabel(host?: Host | null): string | undefined {
  const parts = [host?.instance_type, host?.availability_zone].filter(Boolean);
  return parts.length > 0 ? parts.join(' ') : undefined;
}

/**
 * vCPUs and memory (MiB) for display, e.g. `4 vCPU · 29 GiB`, with memory in `fmtBytes`'s binary
 * units; just the part known when only one is, and undefined when neither is. `separator` joins the
 * two (the attempt picker, itself `·`-separated, uses `, `).
 */
export function resourcesLabel(
  vcpus?: number | null,
  memoryMib?: number | null,
  separator = ' · ',
): string | undefined {
  const parts: string[] = [];
  if (vcpus != null) parts.push(`${Number(vcpus.toFixed(2))} vCPU`);
  if (memoryMib != null) parts.push(fmtBytes(memoryMib * 1024 * 1024));
  return parts.length > 0 ? parts.join(separator) : undefined;
}

/**
 * What a task's Batch job asked for (not what its host has), as `resourcesLabel`; undefined when
 * unknown (e.g. a run recorded before biffix kept the request).
 */
export function requestedLabel(t: {
  requested_vcpus?: number | null;
  requested_memory_mib?: number | null;
}): string | undefined {
  return resourcesLabel(t.requested_vcpus, t.requested_memory_mib);
}

/** What a host can hold (its ECS-registered vCPUs and memory), as `resourcesLabel`. */
export function hostCapacityLabel(host?: Host | null): string | undefined {
  return resourcesLabel(host?.registered_vcpus, host?.registered_memory_mib);
}

/**
 * A one-line summary of an attempt for the attempt picker, e.g.
 * `attempt 1 · failed · spot reclaim · exit 137 · requested 4 vCPU, 8.0 GiB · c8g.16xlarge us-east-1b
 * · spot · 12m`.
 * Parts that are unknown are left out.
 */
export function attemptLabel(a: TaskAttempt): string {
  const parts = [a.attempt != null ? `attempt ${a.attempt}` : 'attempt ?', a.state];
  const ending = terminationLabel(a);
  if (ending) parts.push(ending);
  if (a.exit_code != null) parts.push(`exit ${a.exit_code}`);
  const requested = resourcesLabel(a.requested_vcpus, a.requested_memory_mib, ', ');
  if (requested) parts.push(`requested ${requested}`);
  const host = hostLabel(a.host);
  if (host) parts.push(host);
  const capacity = capacityLabel(a.host);
  if (capacity) parts.push(capacity);
  if (a.started_at != null) parts.push(fmtWallTime(a.started_at, a.finished_at));
  if (a.cost) parts.push(jobCostText(a.cost.usd));
  return parts.join(' · ');
}

/** A human description of a cost estimate's price basis (`JobCost.price_source`). */
export function priceSourceLabel(source: string): string {
  switch (source) {
    case 'on_demand':
      return 'on-demand list price';
    case 'spot_history':
      return 'spot price history for the host type and AZ';
    case 'spot_at_on_demand':
      return 'spot, priced at on-demand (history unavailable; an upper bound)';
    case 'unknown_at_on_demand':
      return 'capacity type unknown, priced at on-demand';
    default:
      return source;
  }
}
