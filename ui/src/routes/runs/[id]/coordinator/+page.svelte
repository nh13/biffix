<script lang="ts">
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import CoordinatorLog from '$lib/runs/CoordinatorLog.svelte';

  const ctx = getRunContext();
</script>

{#if ctx.run}
  <CoordinatorLog run={ctx.run} />
{/if}
