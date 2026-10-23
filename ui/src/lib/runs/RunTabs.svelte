<script lang="ts">
  import { page } from '$app/state';
  import type { RunDetail } from '$lib/api';
  import { activeRunTab, runTabs } from '$lib/runs/runNav';
  import RunActions from '$lib/runs/RunActions.svelte';
  import StatusPill from '$lib/runs/StatusPill.svelte';

  // The run's views as tabs: real links (so middle-click, back/forward and sharing work), the
  // current one marked for assistive tech. Pinned to the top of the page; once the full header has
  // scrolled away (`compact`), it also carries the run's id, state and actions, in the same row so
  // the bar never changes height.
  interface Props {
    run: RunDetail;
    /** How many reports the run has; null while unknown. */
    reports?: number | null;
    /** A resumed run's outputs list total (`RunState.outputsTotal`); null when unknown or none. */
    outputs?: number | null;
    compact?: boolean;
  }
  let { run, reports = null, outputs = null, compact = false }: Props = $props();

  const tabs = $derived(runTabs(run, reports, outputs));
  const active = $derived(activeRunTab(page.url.pathname));
</script>

<div class="bar" class:compact>
  {#if compact}
    <span class="ident">
      <span class="id" title={run.run_id}>{run.run_name || run.run_id}</span>
      <StatusPill state={run.state} />
    </span>
  {/if}
  <nav class="run-tabs" aria-label="Run views">
    {#each tabs as tab (tab.id)}
      <a
        href={tab.href}
        class:active={tab.id === active}
        class:empty={tab.empty}
        aria-current={tab.id === active ? 'page' : undefined}
        data-sveltekit-noscroll
        >{tab.label}{#if tab.count != null}{' '}<span class="count">{tab.count}</span>{/if}</a
      >
    {/each}
  </nav>
  {#if compact}
    <span class="end"><RunActions {run} announce={false} /></span>
  {/if}
</div>

<style>
  .bar {
    position: sticky;
    top: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin: 0 0 1.25rem;
    background: var(--bg-content);
    border-bottom: 1px solid var(--border);
  }
  .bar.compact {
    box-shadow: 0 6px 12px -10px rgb(0 0 0 / 0.35);
  }
  .ident {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    min-width: 0;
    flex-shrink: 1;
  }
  .id {
    font-weight: 600;
    font-size: 0.85rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 16rem;
  }
  .end {
    margin-left: auto;
    flex: none;
  }
  .run-tabs {
    display: flex;
    gap: 0.25rem;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: thin;
  }
  a {
    flex: none;
    padding: 0.45rem 0.7rem;
    font-size: 0.85rem;
    color: var(--text-muted);
    text-decoration: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    white-space: nowrap;
  }
  a:hover {
    color: var(--text);
  }
  a.active {
    color: var(--text);
    font-weight: 600;
    border-bottom-color: var(--accent, #4a7cf5);
  }
  /* Nothing there yet: set apart without fading below readable contrast. */
  a.empty:not(.active) {
    color: var(--text-muted);
    font-style: italic;
  }
  .count {
    margin-left: 0.1rem;
    font-size: 0.72rem;
    font-weight: normal;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
