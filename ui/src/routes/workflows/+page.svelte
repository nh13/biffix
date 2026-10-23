<script lang="ts">
  import { view } from '$lib/state/userPrefs.svelte';
  import PipelineCatalog from '$lib/catalog/PipelineCatalog.svelte';
  import WorkflowTable from '$lib/workflows/WorkflowTable.svelte';
</script>

{#if view.mode === 'lab'}
  <PipelineCatalog />
{:else}
  <WorkflowTable />
{/if}
