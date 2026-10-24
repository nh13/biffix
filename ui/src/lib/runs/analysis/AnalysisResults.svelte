<script lang="ts">
  // *Results* on the Lab analysis page: a card per key result the pipeline declares
  // (a per-sample one folded into one card), each opening in the viewer its format calls for (the
  // output viewer, FastQC's or the genome browser); every file is one link away.
  import type { RunDetail, WorkflowDetail } from '#lib/api/index.js';
  import { currentSubject } from '#lib/api/auth.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import ResultViewer from '#lib/qc/ResultViewer.svelte';
  import { getRunContext } from '#lib/runs/runContext.svelte.js';
  import { isTerminalRun } from '#lib/runs/runState.js';
  import ResultCard from './ResultCard.svelte';
  import type { OpenTarget } from '#lib/qc/open.js';
  import { noResultsReason, resultCards, showsAuthorHint } from '#lib/runs/results.js';

  interface Props {
    run: RunDetail;
    /** The run's version, once the page has loaded it: says whether it declares key results. */
    version: WorkflowDetail | null;
  }
  let { run, version }: Props = $props();

  const ctx = getRunContext();
  const cards = $derived(resultCards(ctx.reports ?? []));
  const ended = $derived(isTerminalRun(run));
  const partial = $derived(ended && run.state !== 'succeeded');
  const filesHref = $derived(`/runs/${encodeURIComponent(run.run_id)}/files`);
  let open = $state<OpenTarget | null>(null);
  // With none to show: why, and whether the hint on declaring them is for this viewer (who
  // registered the version, so can act on it).
  const noResults = $derived(noResultsReason(version));
  const authorHint = $derived(
    noResults === 'undeclared' && showsAuthorHint(version?.created_by, currentSubject(), view.mode),
  );
</script>

<section id="results" aria-labelledby="results-h">
  <div class="head">
    <h2 id="results-h">
      Results{#if partial && cards.length}<span class="note">{' · partial'}</span>{/if}
    </h2>
    <a href={filesHref}>All files →</a>
  </div>
  {#if ctx.reportsError}
    <p class="error" role="alert">
      Couldn't load the results: {ctx.reportsError}
      <button class="link" onclick={() => ctx.reloadReports()}>Try again</button>
    </p>
  {/if}
  {#if ctx.reports === null}
    {#if !ctx.reportsError}<p class="muted">Loading…</p>{/if}
  {:else if cards.length}
    {#if !ended}<p class="muted">More results appear here as the analysis finishes.</p>{/if}
    <div class="cards">
      {#each cards as card (card.key)}
        <ResultCard runId={run.run_id} project={run.project} {card} onopen={(f) => (open = f)} />
      {/each}
    </div>
  {:else if !ended}
    <p class="muted">Results appear here as the analysis finishes.</p>
  {:else if noResults === 'undeclared'}
    <p class="muted">
      This pipeline doesn't declare key results — <a href={filesHref}>all files are here →</a>
    </p>
    {#if authorHint}
      <p class="muted author-hint">
        To show its key results here first, list them under <code>reports:</code> in the pipeline's
        <code>biffix.yaml</code>.
      </p>
    {/if}
  {:else if noResults === 'unwritten'}
    <p class="muted">
      No results were written — <a href={filesHref}>all files are here →</a>
    </p>
  {:else}
    <p class="muted">No key results to show — <a href={filesHref}>all files are here →</a></p>
  {/if}
</section>

{#if open}
  <ResultViewer target={open} onclose={() => (open = null)} />
{/if}

<style>
  section {
    margin-bottom: 1.5rem;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 0.6rem;
  }
  h2 {
    margin: 0;
  }
  .note {
    font-size: 0.8rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  a {
    color: var(--accent);
    font-size: 0.85rem;
  }
  .muted a {
    text-decoration: underline;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(15rem, 100%), 1fr));
    gap: 0.8rem;
  }
  .muted {
    font-size: 0.88rem;
  }
  .author-hint {
    font-size: 0.8rem;
  }
</style>
