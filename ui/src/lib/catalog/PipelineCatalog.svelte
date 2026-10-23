<script lang="ts">
  // The Lab view's pipeline catalog: a card per pipeline (its newest released version), found by what
  // it does rather than by version ids. Pipelines a developer hid from the listing are left out, and
  // so, unless the viewer turns on Developer, are pipelines with no released version.
  import type { Snippet } from 'svelte';
  import { WorkflowCatalog, type WorkflowGroup as Group } from '$lib/workflows/catalog.svelte';
  import PipelineCard from './PipelineCard.svelte';
  import { loadPins, view } from '$lib/state/userPrefs.svelte';
  import { catalogTitle as titleOf, launchVersion } from '$lib/workflows/release';
  import DeveloperToggle from '$lib/workflows/DeveloperToggle.svelte';
  import NothingReleased from '$lib/workflows/NothingReleased.svelte';
  import { plural } from '$lib/util/format';

  interface Props {
    /** The page's heading. */
    title?: string;
    /** The line under it. */
    lede?: string;
    /** Shown between the heading and the line (New run's wizard steps). */
    children?: Snippet;
    /** The experiment a pipeline's Start launches into. */
    experiment?: string | null;
    /** Only this project's pipelines (an experiment's: a run joins one of its own project). */
    project?: string | null;
  }
  let {
    title = 'Pipelines',
    lede = 'Pick the pipeline for your experiment, then add your samples.',
    children,
    experiment = null,
    project = null,
  }: Props = $props();

  let search = $state('');
  let tag = $state<string | null>(null);
  // A tag no longer offered (Developer turned off took its chip) filters nothing any more.
  $effect(() => {
    if (tag !== null && !tags.includes(tag)) tag = null;
  });
  type Sort = 'used' | 'name' | 'updated';
  let sort = $state<Sort>('used');

  const catalog = new WorkflowCatalog(
    (g) => !g.latest.hidden && (!project || g.latest.project === project),
  );
  $effect(() => {
    void loadPins();
    catalog.load();
  });

  const loading = $derived(catalog.loading);
  const error = $derived(catalog.error);
  const groups = $derived(catalog.groups);
  // Pipelines with a released version, or (Developer on) all of them.
  const filtered = $derived(catalog.filtered);
  const listed = $derived(filtered.shown);
  /** The card's version: what its tags and description are read from. */
  const card = (g: Group) => launchVersion(g);
  /** Every tag any pipeline carries, for the filter chips (first spelling wins, by name). */
  const tags = $derived.by(() => {
    const seen = new Map<string, string>();
    for (const g of listed)
      for (const t of card(g).catalog?.tags ?? [])
        if (!seen.has(t.toLowerCase())) seen.set(t.toLowerCase(), t);
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
  });
  const hasTag = (g: Group, t: string) =>
    (card(g).catalog?.tags ?? []).some((x) => x.toLowerCase() === t.toLowerCase());
  const text = (g: Group) => {
    const v = card(g);
    return [g.name, v.title ?? '', v.catalog?.description ?? '', ...(v.catalog?.tags ?? [])]
      .join(' ')
      .toLowerCase();
  };
  const visible = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const shown = listed.filter((g) => (!q || text(g).includes(q)) && (!tag || hasTag(g, tag)));
    const by: Record<Sort, (a: Group, b: Group) => number> = {
      used: (a, b) => b.runCount - a.runCount || titleOf(a).localeCompare(titleOf(b)),
      name: (a, b) => titleOf(a).localeCompare(titleOf(b)),
      updated: (a, b) => card(b).created_at_micros - card(a).created_at_micros,
    };
    return shown.sort(by[sort]);
  });
</script>

<div class="title-row">
  <h1>{title}</h1>
</div>
{@render children?.()}
<p class="lede">{lede}</p>

{#if loading}
  <p class="muted">Loading…</p>
{:else if error}
  <p class="error" role="alert">Couldn't load pipelines: {error}</p>
{:else if groups.length === 0 && project}
  <p class="muted">No pipelines in project {project} yet.</p>
{:else if groups.length === 0}
  <p class="muted">
    No pipelines yet — a developer registers them with <code>biffix workflows register</code>.
  </p>
{:else}
  <div class="controls">
    <input
      class="search"
      type="search"
      placeholder="Search pipelines…"
      aria-label="Search pipelines"
      bind:value={search}
    />
    <label class="sort"
      >Sort
      <select bind:value={sort}>
        <option value="used">Most used</option>
        <option value="name">Name</option>
        <option value="updated">Recently updated</option>
      </select>
    </label>
    <DeveloperToggle />
  </div>
  {#if listed.length === 0}
    <NothingReleased count={groups.length} />
  {:else}
    {#if filtered.hiddenByFilter > 0}
      <p class="muted filter-note" data-testid="developer-note">
        {plural(filtered.hiddenByFilter, 'pipeline')} in development not shown — turn on Developer to
        see {filtered.hiddenByFilter === 1 ? 'it' : 'them'}.
      </p>
    {/if}
    {#if tags.length}
      <div class="chips" role="group" aria-label="Filter by tag">
        <button class:on={tag === null} aria-pressed={tag === null} onclick={() => (tag = null)}
          >All</button
        >
        {#each tags as t (t)}
          <button
            class:on={tag === t}
            aria-pressed={tag === t}
            onclick={() => (tag = tag === t ? null : t)}>{t}</button
          >
        {/each}
      </div>
    {/if}
    {#if view.pinError}<p class="error" role="alert">Couldn't change a pin: {view.pinError}</p>{/if}
    {#if visible.length}
      <div class="grid">
        {#each visible as group (group.key)}<PipelineCard {group} {experiment} />{/each}
      </div>
    {:else}
      <p class="muted">No pipelines match.</p>
    {/if}
  {/if}
{/if}

<style>
  h1 {
    margin: 0;
  }
  .lede {
    color: var(--text-muted);
    margin: 0.3rem 0 1rem;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    align-items: center;
    margin-bottom: 0.6rem;
  }
  .search {
    flex: 1 1 16rem;
    max-width: 24rem;
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
  }
  .sort {
    display: inline-flex;
    gap: 0.4rem;
    align-items: center;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .sort select {
    font: inherit;
    color: var(--text);
    background: var(--bg-content);
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    padding: 0.2rem 0.4rem;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
    margin-bottom: 1rem;
  }
  .chips button {
    font: inherit;
    font-size: 0.78rem;
    padding: 0.15rem 0.7rem;
    border-radius: 999px;
    border: 1px solid var(--control-border);
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .chips button.on {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    border-color: var(--tab-active-bg);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(17rem, 1fr));
    gap: 0.9rem;
    max-width: 1200px;
  }
  .filter-note {
    font-size: 0.78rem;
    margin: 0 0 0.6rem;
  }
</style>
