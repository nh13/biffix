<script lang="ts">
  // An experiment's results across its analyses, in three
  // views: a compact table (an analysis per row, its results as columns grouped by kind), each
  // result with every analysis's card side by side (to compare them), or each analysis with its
  // cards. A sample filter shows one sample's per-sample results everywhere. The view is remembered
  // in this browser; the reports are loaded once for every view (`RunReportList`).
  import type { RunSummary } from '$lib/api';
  import { readPref, writePref } from '$lib/util/storage';
  import type { OpenTarget } from '$lib/qc/open';
  import ExperimentRunResults from './ExperimentRunResults.svelte';
  import ResultCard from '$lib/runs/analysis/ResultCard.svelte';
  import ResultCell from '$lib/runs/analysis/ResultCell.svelte';
  import {
    cardFor,
    initialView,
    kindGroups,
    RESULTS_VIEWS,
    resultSamples,
    type ResultsView,
    type RunCards,
  } from './resultMatrix';
  import { resultCards } from '$lib/runs/results';
  import { RunReportList } from './runReports.svelte';
  import { runName } from '$lib/runs/runName';
  import { runTabHref } from '$lib/runs/runNav';

  interface Props {
    /** The analyses whose results are shown (those that may have some), newest first. */
    runs: RunSummary[];
    /** Show run ids (the Expert view). */
    showId?: boolean;
    /** Lab words (analyses) or Expert words (runs). */
    lab?: boolean;
    onopen: (target: OpenTarget) => void;
  }
  let { runs, showId = false, lab = true, onopen }: Props = $props();
  const runWord = $derived(lab ? 'analysis' : 'run');
  const runsWord = $derived(lab ? 'analyses' : 'runs');

  const uid = $props.id();
  const VIEW_KEY = 'biffix-experiment-results-view';
  let chosen = $state<ResultsView | null>(null);
  const shown = $derived(chosen ?? initialView(readPref(VIEW_KEY), runs.length));
  function pick(v: ResultsView) {
    chosen = v;
    writePref(VIEW_KEY, v);
  }

  const loader = new RunReportList();
  $effect(() => {
    loader.sync(runs);
  });

  const perRun = $derived<RunCards[]>(
    runs.map((r) => ({
      runId: r.run_id,
      cards: resultCards(loader.entries[r.run_id]?.reports ?? []),
    })),
  );
  const groups = $derived(kindGroups(perRun));
  const columns = $derived(groups.flatMap((g) => g.columns));
  const samples = $derived(resultSamples(perRun));
  let sample = $state<string | null>(null);
  // A sample no longer among them (another analysis was added or removed): back to all.
  const activeSample = $derived(sample && samples.includes(sample) ? sample : null);
  const cardsByRun = $derived(new Map(perRun.map((r) => [r.runId, r.cards])));
  const cardsOf = (runId: string) => cardsByRun.get(runId) ?? [];
  const loading = $derived(
    runs.some((r) => loader.entries[r.run_id]?.reports == null && !loader.entries[r.run_id]?.error),
  );
  const failed = $derived(runs.filter((r) => loader.entries[r.run_id]?.error));
</script>

<div class="tools">
  <div class="views" role="group" aria-label="Show results">
    {#each RESULTS_VIEWS as v (v.id)}
      <button
        type="button"
        aria-pressed={shown === v.id}
        class:on={shown === v.id}
        onclick={() => pick(v.id)}>{v.label}</button
      >
    {/each}
  </div>
  {#if samples.length}
    <span class="sample-filter">
      <label for={`${uid}-sample`}>Sample</label>
      <select
        id={`${uid}-sample`}
        value={activeSample ?? ''}
        onchange={(e) => (sample = (e.currentTarget as HTMLSelectElement).value || null)}
      >
        <option value="">All samples</option>
        {#each samples as s (s)}<option value={s}>{s}</option>{/each}
      </select>
    </span>
  {/if}
</div>
{#if activeSample}
  <p class="muted note" role="status">
    Showing {activeSample}'s results in every {runWord}; results for the whole study are shown as
    they are.
  </p>
{/if}

{#if shown === 'analysis'}
  {#each runs as run (run.run_id)}
    <ExperimentRunResults
      {run}
      entry={loader.entries[run.run_id]}
      onretry={() => loader.retry(run.run_id)}
      sample={activeSample}
      {showId}
      {onopen}
    />
  {/each}
{:else}
  {#each failed as run (run.run_id)}
    <p class="error" role="alert">
      Couldn't load the results of {runName(run)}: {loader.entries[run.run_id]?.error}
      <button class="link" onclick={() => loader.retry(run.run_id)}>Try again</button>
    </p>
  {/each}
  {#if columns.length === 0}
    <p class="muted">
      {loading ? 'Loading…' : `None of these ${runsWord} has key results to show yet.`}
    </p>
  {:else if shown === 'table'}
    <div class="scroll">
      <table class="matrix" aria-labelledby={`${uid}-cap`}>
        <caption id={`${uid}-cap`} class="sr-only">Key results of each {runWord}</caption>
        <thead>
          <tr class="kinds">
            <td></td>
            {#each groups as g (g.label)}
              <th scope="colgroup" colspan={g.columns.length}>{g.label}</th>
            {/each}
          </tr>
          <tr>
            <th scope="col">{lab ? 'Analysis' : 'Run'}</th>
            {#each columns as c (c.name)}
              <th scope="col"
                >{c.name}{#if c.perSample}<span class="per">one per sample</span>{/if}</th
              >
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each runs as run (run.run_id)}
            {@const name = runName(run)}
            <tr>
              <th scope="row">
                <a href={runTabHref(run.run_id, 'overview')}>{name}</a>
                {#if showId}<span class="id">{run.run_id}</span>{/if}
              </th>
              {#each columns as c (c.name)}
                {@const card = cardFor(cardsOf(run.run_id), c.name, activeSample)}
                <td>
                  {#if card}
                    <ResultCell runId={run.run_id} {card} analysis={name} {onopen} />
                  {:else if loader.entries[run.run_id]?.reports == null}
                    <span class="muted">…</span>
                  {:else}
                    <span class="none" aria-hidden="true">–</span><span class="sr-only">none</span>
                  {/if}
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else}
    {#each groups as g, gi (g.label)}
      {#each g.columns as c, ci (c.name)}
        <section class="compare" aria-labelledby={`${uid}-r${gi}-${ci}`}>
          <h3 id={`${uid}-r${gi}-${ci}`}>
            {c.name}
            <span class="kind">{g.label}{c.perSample ? ' · one per sample' : ''}</span>
          </h3>
          <div class="cards">
            {#each runs as run (run.run_id)}
              {@const card = cardFor(cardsOf(run.run_id), c.name, activeSample)}
              {#if card}
                <ResultCard
                  runId={run.run_id}
                  project={run.project}
                  {card}
                  heading={runName(run)}
                  {onopen}
                />
              {/if}
            {/each}
          </div>
        </section>
      {/each}
    {/each}
  {/if}
{/if}

<style>
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem 1rem;
    margin: 0 0 0.7rem;
  }
  .views {
    display: inline-flex;
    border: 1px solid var(--control-border);
    border-radius: 999px;
    padding: 2px;
  }
  .views button {
    border: none;
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: 0.78rem;
    padding: 0.2rem 0.75rem;
    border-radius: 999px;
    cursor: pointer;
  }
  .views button.on {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
  }
  .views button:focus-visible,
  .sample-filter select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .sample-filter {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .sample-filter select {
    font: inherit;
    padding: 0.2rem 0.3rem;
    border: 1px solid var(--control-border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .note {
    margin: 0 0 0.6rem;
  }
  .scroll {
    overflow-x: auto;
    border: 1px solid var(--card-border);
    border-radius: 0.55rem;
  }
  .matrix {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
  }
  .matrix th,
  .matrix td {
    padding: 0.4rem 0.6rem;
    text-align: left;
    vertical-align: middle;
    border-bottom: 1px solid var(--border);
  }
  .matrix thead th {
    background: var(--th-bg);
    font-size: 0.75rem;
    font-weight: 600;
    white-space: nowrap;
  }
  .matrix tr.kinds th,
  .matrix tr.kinds td {
    font-size: 0.7rem;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--text-muted);
    background: var(--th-bg);
    border-bottom: none;
  }
  .matrix tbody th {
    font-weight: 600;
    white-space: nowrap;
  }
  .matrix tbody th a {
    color: var(--text);
    text-decoration: underline;
  }
  .per {
    display: block;
    font-size: 0.68rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  .id {
    display: block;
    font-family: var(--mono);
    font-size: 0.7rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  .none {
    color: var(--text-muted);
  }
  .compare {
    margin-bottom: 1.1rem;
  }
  .compare h3 {
    margin: 0 0 0.5rem;
    font-size: 0.92rem;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
  }
  .kind {
    font-size: 0.75rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(15rem, 100%), 1fr));
    gap: 0.8rem;
  }
  .muted {
    margin: 0;
    font-size: 0.85rem;
  }
  .error {
    margin: 0 0 0.5rem;
    font-size: 0.85rem;
  }
</style>
