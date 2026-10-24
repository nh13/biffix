<script lang="ts">
  import { view } from '#lib/state/userPrefs.svelte.js';
  import LabHome from '#lib/home/LabHome.svelte';
  import ExpertHome from '#lib/home/ExpertHome.svelte';
</script>

{#if view.mode === 'lab'}
  <LabHome />
{:else}
  <ExpertHome />
{/if}
