<script lang="ts">
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import JobsView from '$lib/runs/JobsView.svelte';

  const ctx = getRunContext();
</script>

{#if ctx.run}
  <JobsView run={ctx.run} />
{/if}
