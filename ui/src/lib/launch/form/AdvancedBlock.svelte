<script lang="ts">
  // *Advanced*: the sections the author marks advanced, collapsed together under one summary that
  // names each group (a dot on one changed from its defaults), each group collapsed in turn unless
  // it was changed or every parameter is shown.
  import { untrack } from 'svelte';
  import { plural } from '#lib/util/format.js';
  import type { Card } from '../form';
  import FieldControl from './FieldControl.svelte';
  import { launchForm } from './launchForm.svelte';

  let { groups }: { groups: Card[] } = $props();
  const form = launchForm();

  // Each group's open state, once it is the user's (they toggled it) or opened for them. A group
  // opens when every parameter is shown or a field in it changes, and is never closed under the
  // user when that change is undone (focus would be left on a hidden field).
  let opened = $state<Record<string, boolean>>({});
  let wasChanged = new Set<string>();
  $effect(() => {
    const all = form.shownAll;
    const changed = new Set(groups.filter((g) => g.changed).map((g) => g.section.id));
    untrack(() => {
      for (const g of groups) {
        const id = g.section.id;
        if (all || (changed.has(id) && !wasChanged.has(id))) opened[id] = true;
      }
      wasChanged = changed;
    });
  });
</script>

{#if groups.length > 0}
  {@const count = groups.reduce((n, g) => n + g.fields.length, 0)}
  <details class="card advanced" bind:open={form.advancedOpen}>
    <summary>
      <span class="adv-title">Advanced</span>
      <span class="adv-groups">
        {#each groups as g}<span class="chip"
            >{g.section.title}{#if g.changed}<span class="dot" title="Changed from its default"
              ></span>{/if}</span
          >{/each}
      </span>
      <span class="muted adv-count"
        >{plural(groups.length, 'group')} · {plural(count, 'parameter')}</span
      >
    </summary>
    {#each groups as g (g.section.id)}
      <details
        class="group"
        open={opened[g.section.id] ?? (form.shownAll || g.changed)}
        ontoggle={(e) => (opened[g.section.id] = e.currentTarget.open)}
        data-holds={g.fields.map((f) => f.name).join(' ')}
      >
        <summary>
          {g.section.title}{#if g.changed}<span class="dot" title="Changed from its default"
            ></span>{/if}
        </summary>
        {#if g.section.description}<p class="sec-desc">{g.section.description}</p>{/if}
        {#each g.fields as f (f.name)}
          <FieldControl field={f} />
        {/each}
      </details>
    {/each}
  </details>
{/if}

<style>
  .card {
    margin-bottom: 0.9rem;
  }
  .sec-desc {
    margin: 0.2rem 0 0.2rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .advanced > summary {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    cursor: pointer;
  }
  .adv-title {
    font-weight: 600;
  }
  .adv-groups {
    display: inline-flex;
    gap: 0.3rem;
    flex-wrap: wrap;
  }
  .dot {
    display: inline-block;
    width: 0.4rem;
    height: 0.4rem;
    border-radius: 50%;
    background: var(--accent);
    margin-left: 0.3rem;
    vertical-align: middle;
  }
  .adv-count {
    margin-left: auto;
  }
  .group {
    margin-top: 0.6rem;
    padding: 0.2rem 0 0.2rem 0.75rem;
    border-left: 2px solid var(--border);
  }
  .group > summary {
    cursor: pointer;
    font-size: 0.88rem;
    font-weight: 600;
  }
  summary:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .muted {
    font-size: 0.85rem;
  }
</style>
