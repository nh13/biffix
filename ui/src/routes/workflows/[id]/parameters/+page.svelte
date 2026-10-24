<script lang="ts">
  import { getWorkflowContext } from '#lib/workflows/workflowContext.svelte.js';
  import Params from '#lib/workflows/Params.svelte';

  const wf = getWorkflowContext();
</script>

{#if wf.detail}
  <Params formSchema={wf.detail.form_schema} workflowId={wf.detail.id} />
{/if}
