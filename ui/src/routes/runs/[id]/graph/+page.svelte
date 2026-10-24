<script lang="ts">
  import { goto } from '$app/navigation';
  import { getRunContext } from '#lib/runs/runContext.svelte.js';
  import { jobsHref } from '#lib/runs/jobsUrl.js';
  import RunGraph from '#lib/runs/RunGraph.svelte';

  const ctx = getRunContext();

  // A node opens the Jobs view filtered to that step.
  function open(group: string | null) {
    if (!ctx.run || !group) return;
    goto(jobsHref(ctx.run.run_id, { group }));
  }
</script>

{#if ctx.run}
  {#if ctx.run.dag && ctx.run.dag.nodes.length > 0}
    <RunGraph dag={ctx.run.dag} onselect={open} hint="Click a step to open its jobs." />
  {:else}
    <p class="muted">No step graph was recorded for this run.</p>
  {/if}
{/if}
