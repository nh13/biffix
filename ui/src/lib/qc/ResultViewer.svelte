<script lang="ts">
  // Whatever Open on a key result shows (`OpenTarget`): the output viewer, the FastQC viewer or the
  // genome browser. One place, so the analysis page, an experiment's results and QC at a glance
  // open results the same way.
  import OutputViewer from '#lib/components/OutputViewer.svelte';
  import type { OpenTarget } from './open';
  import AlignmentViewer from './AlignmentViewer.svelte';
  import FastqcViewer from './FastqcViewer.svelte';

  let { target, onclose }: { target: OpenTarget; onclose: () => void } = $props();
</script>

{#if target.kind === 'viewer'}
  <OutputViewer file={target.file} {onclose} />
{:else if target.kind === 'fastqc'}
  <FastqcViewer runId={target.runId} output={target.output} title={target.title} {onclose} />
{:else}
  <AlignmentViewer
    runId={target.runId}
    outputId={target.output.id}
    title={target.title}
    {onclose}
  />
{/if}
