<script lang="ts">
  import { getRunContext } from '#lib/runs/runContext.svelte.js';
  import RunCost from '#lib/runs/RunCost.svelte';

  const ctx = getRunContext();
</script>

{#if ctx.run}
  <RunCost run={ctx.run} />
{/if}
