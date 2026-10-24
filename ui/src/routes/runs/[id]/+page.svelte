<script lang="ts">
  import { getRunContext } from '#lib/runs/runContext.svelte.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import AnalysisPage from '#lib/runs/analysis/AnalysisPage.svelte';
  import RunOverview from '#lib/runs/RunOverview.svelte';

  // The run's first page: the Lab view's one analysis page, or the Expert view's Overview tab.
  const ctx = getRunContext();
</script>

{#if ctx.run && view.mode === 'lab'}
  <AnalysisPage run={ctx.run} />
{:else if ctx.run}
  <RunOverview run={ctx.run} reports={ctx.reports} />
{/if}
