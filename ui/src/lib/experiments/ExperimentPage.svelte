<script lang="ts">
  // One experiment, in either view's words: its header (name, description, Start,
  // CSV, Archive), its counts and one total cost, its analyses (the Lab view's rows, the Expert
  // view's table with run ids), the key results across them, and every sample × analysis.
  import type { Experiment, ExperimentDetail } from '$lib/api';
  import { t } from '$lib/view/vocab';
  import type { ViewMode } from '$lib/view/viewRoutes';
  import ResultViewer from '$lib/qc/ResultViewer.svelte';
  import type { OpenTarget } from '$lib/qc/open';
  import RunRow from '$lib/home/RunRow.svelte';
  import {
    anyLive,
    cappedRunsText,
    failedCount,
    mayHaveResults,
    RESULTS_PAGE,
    runCountText,
    samplesSummary,
    sampleTable,
    totalCostText,
  } from './experiments';
  import ExperimentHeader from './ExperimentHeader.svelte';
  import ExperimentResults from './ExperimentResults.svelte';
  import ExperimentRunsTable from './ExperimentRunsTable.svelte';
  import ExperimentSamples from './ExperimentSamples.svelte';

  interface Props {
    detail: ExperimentDetail;
    mode: ViewMode;
    /** An edit was saved: the experiment as it is now. */
    onupdated: (experiment: Experiment) => void;
  }
  let { detail, mode, onupdated }: Props = $props();

  const exp = $derived(detail.experiment);
  const lab = $derived(mode === 'lab');
  const runs = $derived(detail.runs);
  const live = $derived(anyLive(runs));
  // The total still grows while an analysis runs or has jobs not priced yet.
  const cost = $derived(
    totalCostText(detail.total_cost_usd, live || runs.some((r) => r.cost_pending)),
  );
  const table = $derived(sampleTable(detail));
  const runsWord = $derived(t('runsLower', mode));
  // The detail holds the newest runs only (the server caps them); `runs_total` counts them all.
  const runsTotal = $derived(Math.max(detail.runs_total ?? runs.length, runs.length));
  const capped = $derived(runsTotal > runs.length);
  const newestOf = $derived(` (newest ${runs.length} ${runsWord})`);
  const runsListHref = $derived(`/runs?experiment=${encodeURIComponent(exp.id)}`);

  // -- Results across analyses: the newest few at first, more on asking ---------------------------
  let shownResults = $state(RESULTS_PAGE);
  const resultRuns = $derived(runs.filter(mayHaveResults));
  const waitingForResults = $derived(runs.length - resultRuns.length);
  let open = $state<OpenTarget | null>(null);
</script>

<ExperimentHeader {detail} {mode} {onupdated} />

<dl class="stats">
  <div>
    <dt>{t('runs', mode)}</dt>
    <dd>{runsTotal}</dd>
  </div>
  <!-- Counted over the runs the detail holds: the newest ones when there are more. -->
  <div>
    <dt>
      Samples{#if capped}<span class="of">{newestOf}</span>{/if}
    </dt>
    <dd>{table.rows.length}</dd>
  </div>
  <div>
    {#if lab}
      <!-- The caller's own failed analyses not dismissed, as My work counts them. -->
      <dt>Needs attention</dt>
      <dd>{exp.needs_attention ?? 0}</dd>
    {:else}
      <dt>
        Failed{#if capped}<span class="of">{newestOf}</span>{/if}
      </dt>
      <dd>{failedCount(runs)}</dd>
    {/if}
  </div>
  <div>
    <dt>Total cost</dt>
    <dd data-testid="experiment-cost">{cost ?? '--'}</dd>
  </div>
</dl>

<section id="analyses" aria-labelledby="analyses-h">
  <div class="section-head">
    <h2 id="analyses-h">{t('runs', mode)}</h2>
    {#if runs.length}
      <a href={runsListHref}>See them in {t('runs', mode)} →</a>
    {/if}
  </div>
  {#if capped}
    <p class="muted capped" data-testid="runs-capped">
      {cappedRunsText(runs.length, runsTotal, mode)}
      <a href={runsListHref}>See them all in {t('runs', mode)} →</a>
    </p>
  {/if}
  {#if runs.length === 0}
    <p class="muted">
      No {runsWord} in it yet. Start one here, or open an {t('runLower', mode)} and choose
      <em>Add to experiment</em>.
    </p>
  {:else if lab}
    <ul class="rows">
      {#each runs as run (run.run_id)}<RunRow {run} />{/each}
    </ul>
  {:else}
    <ExperimentRunsTable {runs} />
  {/if}
</section>

{#if runs.length}
  <section id="results" aria-labelledby="results-h">
    <h2 id="results-h">Results across {runsWord}</h2>
    {#if resultRuns.length}
      <ExperimentResults
        runs={resultRuns.slice(0, shownResults)}
        showId={!lab}
        {lab}
        onopen={(t) => (open = t)}
      />
    {:else}
      <p class="muted">Results appear here as its {runsWord} finish.</p>
    {/if}
    {#if resultRuns.length > shownResults}
      <button class="btn" onclick={() => (shownResults += RESULTS_PAGE)}
        >Show results of {runCountText(
          Math.min(RESULTS_PAGE, resultRuns.length - shownResults),
          mode,
        )} more</button
      >
    {/if}
    {#if waitingForResults > 0 && resultRuns.length > 0}
      <p class="muted">
        {runCountText(waitingForResults, mode)} still to produce results.
      </p>
    {/if}
  </section>
{/if}

{#if table.rows.length}
  <section id="samples" aria-labelledby="samples-h">
    <h2 id="samples-h">
      Samples <span class="count"
        >{samplesSummary(table.rows.length, table.columns.length, mode)}</span
      >
    </h2>
    <ExperimentSamples {table} />
  </section>
{/if}

<p class="people">
  {#if exp.created_by_label || exp.created_by}{`Made by ${exp.created_by_label ?? exp.created_by}. `}{/if}Everyone
  with access to project {exp.project} can see this experiment; anyone who can start
  {runsWord} there can change it.
</p>

{#if open}
  <ResultViewer target={open} onclose={() => (open = null)} />
{/if}

<style>
  .stats {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(9rem, 100%), 1fr));
    gap: 0.6rem;
    margin: 0 0 1.5rem;
  }
  .stats div {
    padding: 0.6rem 0.9rem;
    border: 1px solid var(--card-border);
    border-radius: 0.55rem;
  }
  .stats dt {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
  .stats .of {
    text-transform: none;
    letter-spacing: 0;
  }
  .stats dd {
    margin: 0.15rem 0 0;
    font-size: 1.3rem;
    font-weight: 600;
  }
  section {
    margin-bottom: 1.6rem;
  }
  section h2 {
    margin: 0 0 0.6rem;
  }
  .section-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.4rem 1rem;
    margin-bottom: 0.6rem;
  }
  .section-head h2 {
    margin: 0;
  }
  .capped {
    margin: 0 0 0.6rem;
    font-size: 0.88rem;
  }
  .capped a {
    color: var(--accent);
  }
  .section-head a {
    color: var(--accent);
    font-size: 0.85rem;
  }
  .rows {
    display: grid;
    gap: 0.5rem;
    margin: 0;
    padding: 0;
  }
  .count {
    font-weight: 400;
    color: var(--text-muted);
    font-size: 0.82rem;
  }
  .people {
    font-size: 0.8rem;
    color: var(--text-muted);
    border-top: 1px solid var(--border);
    padding-top: 0.8rem;
  }
</style>
