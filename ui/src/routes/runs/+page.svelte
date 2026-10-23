<script lang="ts">
  // The runs list: one `RunsList` (filter, scopes, search, paging, the live poll, dismissals and
  // sort) shown by the view's presentation: the Lab view's analyses or the Expert view's table.
  // The filter tabs, search, scope chips and pager around it are this page's, in either view.
  import { view } from '$lib/state/userPrefs.svelte';
  import { t } from '$lib/view/vocab';
  import { dismissAllLabel } from '$lib/runs/analysis/dismiss.svelte';
  import { RunsList } from '$lib/runs/runsList.svelte';
  import { nothingStartedYet } from '$lib/runs/runsEmpty';
  import RunsFilterTabs from '$lib/runs/RunsFilterTabs.svelte';
  import AnalysesList from '$lib/runs/AnalysesList.svelte';
  import RunsTable from '$lib/runs/RunsTable.svelte';
  import { experimentHref } from '$lib/experiments/experiments';
  import { workflowHref } from '$lib/workflows/workflowNav';

  const list = new RunsList();
  const lab = $derived(list.lab);
  const filter = $derived(list.filter);
  const ownOnly = $derived(list.ownOnly);
  const experimentScope = $derived(list.experimentScope);
  const workflowScope = $derived(list.workflowScope);
  const listed = $derived(list.listed);
  const range = $derived(list.range);
  const startedNothing = $derived(
    nothingStartedYet({
      lab,
      showEveryone: list.showEveryone,
      experimentScope,
      workflowScope,
      filter,
      search: list.debounced,
    }),
  );
  const shortId = (s: string) => (s.length > 22 ? `${s.slice(0, 22)}…` : s);
</script>

<div class="title-row">
  <h1>{t('runs', view.mode)}</h1>
  <a class="new-run" href="/runs/new">{t('newRun', view.mode)}</a>
</div>

<div class="controls">
  <RunsFilterTabs {list} />
  <input
    class="search"
    type="search"
    placeholder={`Search ${t('runsLower', view.mode)}…`}
    aria-label={`Search ${t('runsLower', view.mode)}`}
    bind:value={list.search}
  />
</div>

{#if ownOnly}
  <div class="own-only">
    <p id="attention-head" tabindex="-1">Your {t('runsLower', view.mode)} that need attention</p>
    {#if listed.runs.length}
      <button
        type="button"
        class="dismiss"
        data-testid="dismiss-all"
        aria-label={`${dismissAllLabel(listed.runs.length, listed.total)} that need attention`}
        onclick={() => list.dismissPage()}
        >{dismissAllLabel(listed.runs.length, listed.total)}</button
      >
    {/if}
  </div>
  {#if list.dismissals.error}
    <p class="error" role="alert">Couldn't dismiss: {list.dismissals.error}</p>
  {/if}
  <p class="sr-only" aria-live="polite">{list.dismissals.announcement}</p>
{/if}

{#if workflowScope}
  <div class="scope">
    Scoped to workflow <a href={workflowHref(workflowScope)} class="mono"
      >{shortId(workflowScope)}</a
    >
    <button
      class="clear"
      onclick={() => list.clearWorkflowScope()}
      aria-label={`Remove the filter: workflow ${workflowScope}`}>clear ✕</button
    >
  </div>
{/if}

{#if experimentScope}
  <div class="scope" data-testid="experiment-chip">
    In experiment <a href={experimentHref(experimentScope)}
      >{list.experimentName ?? experimentScope}</a
    >
    <button
      class="clear"
      onclick={() => list.clearExperimentScope()}
      aria-label={`Remove the filter: experiment ${list.experimentName ?? experimentScope}`}
      >✕</button
    >
  </div>
{/if}

{#if list.loading}
  <p class="muted">Loading…</p>
{:else if list.error}
  <p class="error">Couldn't load {t('runsLower', view.mode)}: {list.error}</p>
{:else if listed.runs.length === 0}
  {#if startedNothing}
    <p class="muted">
      You haven't started an analysis yet. <a href="/runs/new">Start an analysis →</a>
    </p>
  {:else}
    <p class="muted">No {t('runsLower', view.mode)} match this filter.</p>
  {/if}
{:else}
  {#if lab}
    <AnalysesList {list} />
  {:else}
    <RunsTable {list} />
  {/if}
  <div class="pager">
    <span class="range">
      {range.start}–{range.end} of {listed.total}
      {#if list.sortIsPageLocal}<span
          class="page-local"
          title="Paging is server-side; column sort only reorders the rows on this page"
          >· sorted within this page</span
        >{/if}
    </span>
    <div class="pager-btns">
      <button disabled={list.pageIndex === 0} onclick={() => list.prev()}>← Prev</button>
      <button disabled={range.end >= listed.total} onclick={() => list.next()}>Next →</button>
    </div>
  </div>
{/if}

<style>
  .title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .new-run {
    padding: 0.4rem 0.9rem;
    border-radius: 0.45rem;
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    text-decoration: none;
    font-size: 0.85rem;
    white-space: nowrap;
  }
  .new-run:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
    margin-bottom: 0.75rem;
  }
  .own-only {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin: 0 0 0.75rem;
    color: var(--text-muted);
  }
  .own-only p {
    margin: 0;
  }
  .dismiss {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text-muted);
    font-size: 0.78rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .dismiss:hover {
    background: var(--hover);
  }
  .dismiss:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .scope {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .scope .mono {
    font-family: var(--mono);
  }
  .scope .clear {
    padding: 0.1rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.72rem;
  }
  .search {
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font-size: 0.85rem;
    min-width: 200px;
  }
  .pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 0.75rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .page-local {
    margin-left: 0.4rem;
    font-style: italic;
    cursor: help;
  }
  .pager-btns {
    display: flex;
    gap: 0.4rem;
  }
  .pager-btns button {
    padding: 0.25rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .pager-btns button:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
