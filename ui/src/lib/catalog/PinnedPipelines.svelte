<script lang="ts">
  // The viewer's pinned pipelines as a plain list on Settings, each with Unpin: the pipeline's
  // friendly title when it is in the catalog, else its name read back from the pin (a pipeline
  // since removed or hidden can still be unpinned).
  import { loadPins, togglePin, view } from '$lib/state/userPrefs.svelte';
  import { WorkflowCatalog } from '$lib/workflows/catalog.svelte';
  import { pinName } from '$lib/workflows/version';
  import { catalogTitle } from '$lib/workflows/release';

  const catalog = new WorkflowCatalog();
  $effect(() => {
    void loadPins();
    catalog.load();
  });
  const groups = $derived(new Map(catalog.groups.map((g) => [g.key, g])));

  /** A pin's name: the catalog's title, else the name in its key; gone only once the list says so. */
  function nameOf(key: string): { title: string; gone: boolean } {
    const g = groups.get(key);
    const status = catalog.pinStatus(key);
    if (status === 'listed' && g) return { title: catalogTitle(g), gone: false };
    return { title: pinName(key), gone: status === 'gone' };
  }
</script>

{#if !view.pinsLoaded}
  <p class="muted">Loading…</p>
{:else if view.pinned.length === 0}
  <p class="muted" data-testid="no-pins">No pinned pipelines.</p>
{:else}
  <ul class="pins" aria-label="Pinned pipelines">
    {#each view.pinned as key (key)}
      {@const pin = nameOf(key)}
      <li>
        <span
          >{pin.title}{#if pin.gone}<span class="muted">
              · no longer in the catalog</span
            >{/if}</span
        >
        <button type="button" class="ghost" onclick={() => togglePin(key)}
          >Unpin<span class="sr-only"> {pin.title}</span></button
        >
      </li>
    {/each}
  </ul>
{/if}
{#if catalog.error && view.pinned.length > 0}<p class="error" role="alert">
    Couldn't load the pipelines: {catalog.error}
  </p>{/if}
{#if view.pinError}<p class="error" role="alert">Couldn't change a pin: {view.pinError}</p>{/if}

<style>
  .pins {
    display: grid;
    gap: 0.3rem;
    max-width: 32rem;
    margin: 0.3rem 0 0;
    padding: 0;
    list-style: none;
  }
  .pins li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.8rem;
    padding: 0.3rem 0.6rem;
    border: 1px solid var(--card-border);
    border-radius: 0.4rem;
    font-size: 0.85rem;
  }
  .muted {
    font-size: 0.85rem;
  }
  .ghost {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .ghost:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .error {
    font-size: 0.85rem;
  }
</style>
