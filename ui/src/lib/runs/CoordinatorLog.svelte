<script lang="ts">
  import { getCoordinatorLogs, type RunDetail } from '#lib/api/index.js';
  import { isLiveRun } from '#lib/runs/runState.js';
  import LogStream from '#lib/components/LogStream.svelte';

  // The coordinator job's log, live-tailed while the run is going.
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();
</script>

<section class="coordinator-log">
  <h2>Coordinator log</h2>
  {#if run.coordinator_job_id}
    {@const runId = run.run_id}
    <LogStream
      fetchPage={(opts) => getCoordinatorLogs(runId, opts)}
      active={isLiveRun(run)}
      pollMs={5000}
      emptyMessage={run.state === 'submitted'
        ? "The coordinator hasn't started yet."
        : isLiveRun(run)
          ? "The coordinator's log hasn't appeared yet."
          : 'No log stream was recorded for this run — open the Batch job in AWS.'}
    />
  {:else}
    <p class="muted">The coordinator hasn't been launched yet.</p>
  {/if}
</section>

<style>
  .coordinator-log {
    margin-bottom: 1.25rem;
  }
</style>
