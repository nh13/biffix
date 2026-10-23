<script lang="ts">
  // The runs list's filter tabs, in the view's words: the Lab view's Mine | Everyone's and its plain
  // filters, or the Expert view's run states.
  import { LAB_FILTERS, RUN_FILTERS, type RunsList } from './runsList.svelte';

  let { list }: { list: RunsList } = $props();
</script>

{#if list.lab}
  {#if !list.ownOnly && !list.experimentScope}
    <div class="tabs" role="group" aria-label="Whose analyses">
      <button
        class:active={!list.everyone}
        aria-pressed={!list.everyone}
        onclick={() => (list.everyone = false)}>Mine</button
      >
      <button
        class:active={list.everyone}
        aria-pressed={list.everyone}
        onclick={() => (list.everyone = true)}>Everyone’s</button
      >
    </div>
  {/if}
  <div class="tabs">
    {#each LAB_FILTERS as { f, label }}
      <button
        class:active={list.filter === f}
        aria-pressed={list.filter === f}
        onclick={() => list.setFilter(f)}>{label}</button
      >
    {/each}
  </div>
{:else}
  <div class="tabs">
    {#each RUN_FILTERS as f}
      <button
        class:active={list.filter === f}
        aria-pressed={list.filter === f}
        onclick={() => list.setFilter(f)}>{f}</button
      >
    {/each}
  </div>
{/if}
