<script lang="ts">
  // My work's pinned pipelines: the user's pins that name a registered, listed pipeline, in pin
  // order, as cards. A pin whose pipeline is gone or hidden is listed apart, so it can still be
  // unpinned.
  import PipelineCard from '#lib/catalog/PipelineCard.svelte';
  import { togglePin, view } from '#lib/state/userPrefs.svelte.js';
  import type { WorkflowCatalog, WorkflowGroup } from '#lib/workflows/catalog.svelte.js';
  import { pinName } from '#lib/workflows/version.js';

  let { catalog }: { catalog: WorkflowCatalog } = $props();

  const groups = $derived(new Map(catalog.groups.map((g) => [g.key, g])));
  const pinned = $derived(
    view.pinned
      .filter((k) => catalog.pinStatus(k) === 'listed')
      .map((k) => groups.get(k))
      .filter((g): g is WorkflowGroup => !!g),
  );
  /** Pins that no longer name a listed pipeline (once the list has loaded, empty or not). */
  const stalePins = $derived(view.pinned.filter((k) => catalog.pinStatus(k) === 'gone'));
</script>

<section aria-labelledby="pinned-h">
  <h2 id="pinned-h">Pinned pipelines</h2>
  {#if catalog.error}
    <p class="error">Couldn't load your pinned pipelines: {catalog.error}</p>
  {:else if catalog.loading && view.pinned.length > 0}
    <p class="muted">Loading…</p>
  {:else if pinned.length}
    <div class="pins">
      {#each pinned as group (group.key)}<PipelineCard {group} compact />{/each}
    </div>
  {:else if view.pinned.length === 0}
    <p class="muted">
      Pin a pipeline to keep it on My work: star one on the <a href="/workflows">Pipelines</a> page.
    </p>
  {/if}
  {#if stalePins.length}
    <ul class="stale">
      {#each stalePins as key (key)}
        <li>
          {pinName(key)} is no longer in the catalog.
          <button class="link" onclick={() => togglePin(key)}>Unpin</button>
        </li>
      {/each}
    </ul>
  {/if}
  {#if view.pinError}<p class="error" role="alert">Couldn't change a pin: {view.pinError}</p>{/if}
</section>

<style>
  section {
    max-width: 1100px;
    margin-bottom: 1.5rem;
  }
  h2 {
    font-size: 1rem;
    margin: 0 0 0.6rem;
  }
  /* A link inside running text is underlined, not told apart by colour alone. */
  .muted a {
    color: var(--accent);
    text-decoration: underline;
  }
  .stale {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    list-style: none;
    padding: 0;
    margin: 0.5rem 0 0;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .pins {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(15rem, 1fr));
    gap: 0.75rem;
  }
</style>
