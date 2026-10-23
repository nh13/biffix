import { describe, expect, it } from 'vitest';

import type { TaskAttempt } from '$lib/api';
import {
  attemptLabel,
  capacityLabel,
  hostCapacityLabel,
  hostLabel,
  lifecycleLabel,
  priceSourceLabel,
  requestedLabel,
  resourcesLabel,
  terminationLabel,
  verdictLabel,
} from './attempts';

const base: TaskAttempt = { batch_job_id: 'job-1', state: 'running' };

describe('attemptLabel', () => {
  it('summarizes a reclaimed attempt', () => {
    expect(
      attemptLabel({
        ...base,
        attempt: 1,
        state: 'failed',
        termination_category: 'spot',
        exit_code: 137,
        started_at: 1_000,
        finished_at: 1_000 + 12 * 60,
      }),
    ).toBe('attempt 1 · failed · spot reclaim · exit 137 · 12m');
  });

  it('leaves out what is unknown', () => {
    expect(attemptLabel({ ...base, attempt: 2 })).toBe('attempt 2 · running');
    expect(attemptLabel(base)).toBe('attempt ? · running');
  });

  it('keeps exit 0', () => {
    expect(attemptLabel({ ...base, attempt: 2, state: 'succeeded', exit_code: 0 })).toBe(
      'attempt 2 · succeeded · exit 0',
    );
  });
});

describe('host labels', () => {
  it('shows type, AZ and capacity when known', () => {
    const host = {
      instance_type: 'c8g.16xlarge',
      availability_zone: 'us-east-1b',
      capacity: 'spot',
      capacity_source: 'instance',
    };
    expect(hostLabel(host)).toBe('c8g.16xlarge us-east-1b');
    expect(capacityLabel(host)).toBe('spot');
    expect(lifecycleLabel('on_demand')).toBe('on-demand');
    expect(lifecycleLabel('capacity_block')).toBe('capacity-block');
    expect(lifecycleLabel(null)).toBeUndefined();
    expect(attemptLabel({ ...base, attempt: 1, state: 'failed', exit_code: 137, host })).toBe(
      'attempt 1 · failed · exit 137 · c8g.16xlarge us-east-1b · spot',
    );
  });

  it('shows the queue-derived capacity alone when the host is unknown', () => {
    const host = { queue: 'q', capacity: 'on_demand', capacity_source: 'queue' };
    expect(hostLabel(host)).toBeUndefined();
    expect(capacityLabel(host)).toBe('on-demand');
    expect(attemptLabel({ ...base, attempt: 1, host })).toBe('attempt 1 · running · on-demand');
  });

  it('is empty without a host', () => {
    expect(hostLabel(undefined)).toBeUndefined();
    expect(capacityLabel(null)).toBeUndefined();
  });
});

describe('verdictLabel', () => {
  it("says the engine failed the task, with the engine's reason", () => {
    expect(verdictLabel({ verdict_reason: 'Missing output file(s) `A.bam`' })).toBe(
      'Failed by the workflow engine: Missing output file(s) `A.bam`',
    );
  });

  it('is absent when the engine gave no verdict', () => {
    expect(verdictLabel({})).toBeUndefined();
    expect(verdictLabel({ verdict_reason: null })).toBeUndefined();
    expect(verdictLabel({ verdict_reason: '  ' })).toBeUndefined();
  });
});

describe('terminationLabel', () => {
  it('calls a warned host termination a spot reclaim', () => {
    expect(
      terminationLabel({ termination_category: 'host_terminated', spot_warned_at: 1_000 }),
    ).toBe('spot reclaim');
    expect(
      attemptLabel({
        ...base,
        attempt: 1,
        state: 'failed',
        termination_category: 'host_terminated',
        spot_warned_at: 1_000,
      }),
    ).toBe('attempt 1 · failed · spot reclaim');
  });

  it('ranks causes as the failure diagnosis does: memory, then a reclaim, then the rest', () => {
    // A try warned of a reclaim that ran out of memory first ended for lack of memory.
    expect(terminationLabel({ termination_category: 'oom', spot_warned_at: 1_000 })).toBe('oom');
    // A warning outranks the categories after it, as on the server.
    expect(
      terminationLabel({ termination_category: 'missing_output', spot_warned_at: 1_000 }),
    ).toBe('spot reclaim');
    expect(terminationLabel({ termination_category: 'spot' })).toBe('spot reclaim');
  });

  it("otherwise shows Batch's category, in words", () => {
    expect(terminationLabel({ termination_category: 'oom' })).toBe('oom');
    expect(terminationLabel({ termination_category: 'host_terminated' })).toBe('host terminated');
    expect(terminationLabel({ termination_category: 'missing_output' })).toBe('missing output');
    expect(terminationLabel({ termination_category: 'run_ended' })).toBe(
      'no result reported (run ended)',
    );
    expect(terminationLabel({})).toBeUndefined();
  });
});

describe('requested resources', () => {
  it('formats vCPUs and memory in the shared binary units', () => {
    expect(requestedLabel({ requested_vcpus: 4, requested_memory_mib: 30_000 })).toBe(
      '4 vCPU · 29 GiB',
    );
    expect(requestedLabel({ requested_vcpus: 0.25, requested_memory_mib: 512 })).toBe(
      '0.25 vCPU · 512 MiB',
    );
    expect(requestedLabel({ requested_vcpus: 1, requested_memory_mib: 1024 })).toBe(
      '1 vCPU · 1.0 GiB',
    );
  });

  it('shows the part that is known, and nothing when neither is', () => {
    expect(requestedLabel({ requested_vcpus: 2 })).toBe('2 vCPU');
    expect(requestedLabel({ requested_memory_mib: 2048 })).toBe('2.0 GiB');
    expect(requestedLabel({})).toBeUndefined();
    expect(requestedLabel({ requested_vcpus: null, requested_memory_mib: null })).toBeUndefined();
  });

  it('formats memory sizes, choosing the unit after rounding', () => {
    expect(resourcesLabel(null, 1023)).toBe('1023 MiB');
    // Just under 1 GiB reads as 1.0 GiB, not "1024 MiB".
    expect(resourcesLabel(null, 1023.6)).toBe('1.0 GiB');
    expect(resourcesLabel(null, 7680)).toBe('7.5 GiB');
    expect(resourcesLabel(null, 126_000)).toBe('123 GiB');
    expect(resourcesLabel(null, 2_097_152)).toBe('2.0 TiB');
  });

  it('formats a host capacity the same way', () => {
    expect(hostCapacityLabel({ registered_vcpus: 64, registered_memory_mib: 126_000 })).toBe(
      '64 vCPU · 123 GiB',
    );
    expect(hostCapacityLabel({})).toBeUndefined();
    expect(hostCapacityLabel(null)).toBeUndefined();
  });

  it('appears in the attempt summary', () => {
    expect(
      attemptLabel({
        ...base,
        attempt: 2,
        requested_vcpus: 4,
        requested_memory_mib: 8192,
      }),
    ).toBe('attempt 2 · running · requested 4 vCPU, 8.0 GiB');
  });
});

describe('priceSourceLabel', () => {
  it("words each of the pricing's price sources, and shows one it does not know as sent", () => {
    expect(priceSourceLabel('on_demand')).toBe('on-demand list price');
    expect(priceSourceLabel('spot_history')).toBe('spot price history for the host type and AZ');
    expect(priceSourceLabel('spot_at_on_demand')).toBe(
      'spot, priced at on-demand (history unavailable; an upper bound)',
    );
    expect(priceSourceLabel('unknown_at_on_demand')).toBe(
      'capacity type unknown, priced at on-demand',
    );
    expect(priceSourceLabel('reserved')).toBe('reserved');
  });
});
