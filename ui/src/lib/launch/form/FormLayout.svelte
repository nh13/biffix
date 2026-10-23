<script lang="ts">
  // The Expert view's launch form: examples, *Import YAML* and *Show all parameters* on top, then
  // the cards in the author's section order, the advanced sections collapsed together, *Run
  // details*, and a sticky footer that names what is left to fix and launches.
  import AdvancedBlock from './AdvancedBlock.svelte';
  import ExamplesMenu from './ExamplesMenu.svelte';
  import FormCard from './FormCard.svelte';
  import LaunchFooter from './LaunchFooter.svelte';
  import ParamsImport from './ParamsImport.svelte';
  import RunDetailsCard from './RunDetailsCard.svelte';
  import SectionCards from './SectionCards.svelte';
  import { launchForm } from './launchForm.svelte';

  const form = launchForm();
</script>

<div class="start-row">
  <div class="start-left">
    {#if form.examples.length > 0}
      {#if !form.bannerDismissed && !form.dirty && !form.from}
        <div class="first-time">
          <span>New to this workflow? Start from an example, then swap in your own inputs.</span>
          <button class="close" aria-label="Dismiss" onclick={() => form.dismissBanner()}>×</button>
        </div>
      {/if}
      <ExamplesMenu />
    {:else}
      <span class="muted">This workflow has no examples yet.</span>
    {/if}
  </div>
  <div class="start-right">
    <button
      class="btn btn-compact"
      onclick={() => (form.importOpen = !form.importOpen)}
      aria-expanded={form.importOpen}>Import YAML</button
    >
    <label class="toggle">
      <input
        type="checkbox"
        role="switch"
        checked={form.showAll}
        onchange={() => form.toggleShowAll()}
      />
      Show all parameters
    </label>
  </div>
</div>

{#if form.importOpen}
  <ParamsImport />
{/if}

{#if form.loadingRun}
  <p class="muted">Loading the earlier run…</p>
{:else if form.schema.fields.length === 0}
  <FormCard title="Parameters">
    <p class="muted">
      This version declares no parameters. Import a params file above if the workflow needs any, or
      launch it as it is.
    </p>
  </FormCard>
{:else}
  <SectionCards cards={form.lay.cards} />
  <AdvancedBlock groups={form.lay.advanced} />
{/if}

<RunDetailsCard />

<LaunchFooter />

<style>
  .start-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.75rem;
    flex-wrap: wrap;
    margin-bottom: 0.9rem;
  }
  .start-left {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .start-right {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .first-time {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 0.7rem;
    border-radius: 0.45rem;
    border-left: 3px solid var(--accent);
    background: var(--hover);
    font-size: 0.85rem;
  }
  .close {
    border: none;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.82rem;
    color: var(--text);
    cursor: pointer;
  }
  .muted {
    font-size: 0.85rem;
  }
</style>
