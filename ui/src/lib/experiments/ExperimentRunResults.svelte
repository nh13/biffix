<script lang="ts">
  // One analysis's key results on an experiment's page: its name, then a card per result it
  // declares (the analysis page's cards, `ResultCard`), one sample's files of a per-sample result
  // when a sample is picked. Its reports are loaded by the page (`RunReportList`), for every view.
  import type { RunSummary } from '#lib/api/index.js';
  import ResultCard from '#lib/runs/analysis/ResultCard.svelte';
  import type { OpenTarget } from '#lib/qc/open.js';
  import { forSample } from './resultMatrix';
  import { resultCards } from '#lib/runs/results.js';
  import type { RunReportsEntry } from './runReports.svelte';
  import { runName } from '#lib/runs/runName.js';
  import { runTabHref } from '#lib/runs/runNav.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { t } from '#lib/view/vocab.js';

  interface Props {
    run: RunSummary;
    /** Its reports as loaded (none yet: loading). */
    entry: RunReportsEntry | undefined;
    /** Ask for its reports again. */
    onretry: () => void;
    /** Show only this sample's files of a per-sample result. */
    sample?: string | null;
    /** Show the run's id (the Expert view). */
    showId?: boolean;
    onopen: (target: OpenTarget) => void;
  }
  let { run, entry, onretry, sample = null, showId = false, onopen }: Props = $props();

  const uid = $props.id();
  const reports = $derived(entry?.reports ?? null);
  const error = $derived(entry?.error ?? null);
  const all = $derived(resultCards(reports ?? []));
  const cards = $derived(
    all.map((c) => forSample(c, sample)).filter((c): c is NonNullable<typeof c> => c != null),
  );
  const href = $derived(runTabHref(run.run_id, 'overview'));
  const name = $derived(runName(run));
  // Its files: the Lab view's file list, or the Expert view's Outputs tab.
  const filesHref = $derived(`${href}/${view.mode === 'lab' ? 'files' : 'outputs'}`);
</script>

<section class="run-results" aria-labelledby={`${uid}-h`}>
  <h3 id={`${uid}-h`}>
    <a {href}>{name}</a>{#if showId}<span class="id mono">{run.run_id}</span>{/if}
  </h3>
  {#if error}
    <p class="error" role="alert">
      Couldn't load its results: {error}
      <button class="link" onclick={onretry}>Try again</button>
    </p>
  {:else if reports === null}
    <p class="muted">Loading…</p>
  {:else if cards.length}
    <div class="cards">
      {#each cards as card (card.key)}
        <ResultCard runId={run.run_id} project={run.project} {card} {onopen} />
      {/each}
    </div>
  {:else if sample && all.length}
    <p class="muted">
      No results for {sample} in this {t('runLower', view.mode)}.
    </p>
  {:else if run.terminal === false}
    <p class="muted">Its results appear here as it finishes.</p>
  {:else}
    <!-- No version read here: whether the pipeline declares results or this run wrote none is
         unknown, so this says neither. -->
    <p class="muted">No key results to show — <a href={filesHref}>all files are here →</a></p>
  {/if}
</section>

<style>
  .run-results {
    margin-bottom: 1.1rem;
  }
  h3 {
    margin: 0 0 0.5rem;
    font-size: 0.92rem;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
  }
  h3 a {
    color: var(--text);
    text-decoration: underline;
  }
  .id {
    font-size: 0.72rem;
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
  .muted a {
    color: var(--accent);
    text-decoration: underline;
  }
  .error {
    margin: 0;
    font-size: 0.85rem;
  }
</style>
