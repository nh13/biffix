<script lang="ts">
  // An analysis's files in the Lab view (*All files*). It stands for the Expert view's Outputs tab:
  // in the Expert view this URL opens that tab instead.
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import { expertEquivalent } from '$lib/view/viewRoutes';
  import { view } from '$lib/state/userPrefs.svelte';
  import LabFiles from '$lib/runs/analysis/LabFiles.svelte';

  const ctx = getRunContext();

  $effect(() => {
    if (view.mode === 'expert') goto(expertEquivalent(page.url.pathname), { replaceState: true });
  });
</script>

{#if ctx.run && view.mode === 'lab'}
  <LabFiles run={ctx.run} />
{/if}
