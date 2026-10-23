<script lang="ts">
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import RunParams from '$lib/runs/RunParams.svelte';

  const ctx = getRunContext();
</script>

{#if ctx.run}
  <RunParams run={ctx.run} />
{/if}
