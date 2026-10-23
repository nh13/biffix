<script lang="ts">
  // The Lab view's home, "My work": one way to start, then what needs a look, what is running and
  // what just finished — the caller's own analyses only — and the experiments they are in.
  import { listExperiments, listRuns, type Experiment, type RunSummary } from '$lib/api';
  import { WorkflowCatalog } from '$lib/workflows/catalog.svelte';
  import { loadPins } from '$lib/state/userPrefs.svelte';
  import PinnedSection from './PinnedSection.svelte';
  import Welcome from './Welcome.svelte';
  import { PollNudge, startPoll } from '$lib/util/poll';
  import RunRow from './RunRow.svelte';
  import { runName } from '$lib/runs/runName';
  import ExperimentRow from '$lib/experiments/ExperimentRow.svelte';
  import type { Snippet } from 'svelte';
  import { TOUR_ANCHOR } from '$lib/shell/tour';
  import { Dismissals, dismissAllLabel, withoutDismissed } from '$lib/runs/analysis/dismiss.svelte';
  import {
    choiceAfter,
    EXPERIMENTS_ARCHIVED,
    EXPERIMENTS_SEE_ALL,
    rememberChoice,
    rememberedChoice,
    rowOpen,
    ROW_LIMIT,
    seeAllHref,
    workSummary,
    WORK_ROWS,
    type AccordionRow,
    type RowChoice,
    type WorkRow,
  } from './myWork';

  // The three rows' analyses: the five each lists, with the server's true count. Needs attention's
  // less those dismissed here; its dismiss button names exactly the rows it shows.
  type Row = { runs: RunSummary[]; total: number };
  const NONE: Row = { runs: [], total: 0 };
  let fetched = $state<Record<WorkRow, Row>>({ attention: NONE, running: NONE, finished: NONE });
  const dismissals = new Dismissals();
  const rows = $derived<Record<WorkRow, Row>>({
    ...fetched,
    attention: withoutDismissed(fetched.attention, dismissals.hidden),
  });
  let started = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);
  // A dismissal refreshes the rows (the next ones that need a look) and the experiments (their
  // needs-attention flag) at once, rather than at their next poll.
  const analysesNudge = new PollNudge();
  const experimentsNudge = new PollNudge();
  let analysesStale = $state<string | null>(null);

  $effect(() =>
    startPoll({
      fetch: () =>
        Promise.all([
          listRuns('all', { attention: true, limit: ROW_LIMIT }),
          listRuns('active', { mine: true, limit: ROW_LIMIT }),
          listRuns('succeeded', { mine: true, limit: ROW_LIMIT }),
          // Whether the caller has any analysis at all (a read failure or a cancel is in no row).
          listRuns('all', { mine: true, limit: 1 }),
        ]),
      onData: ([attention, running, finished, any]) => {
        started = any.total > 0;
        fetched = { attention, running, finished };
        error = null;
        analysesStale = null;
        // More need a look than when the viewer closed Needs attention: it opens again.
        const shown = withoutDismissed(attention, dismissals.hidden).total;
        const choice = choiceAfter('attention', choices.attention, shown);
        if (choice !== choices.attention) {
          choices.attention = choice;
          rememberChoice('attention', choice);
        }
      },
      onError: (m) => (error = m),
      // A failed refresh keeps the last rows read, and says so.
      onRefreshError: (m) => (analysesStale = m),
      onSettled: () => (loading = false),
      intervalMs: 10_000,
      nudge: analysesNudge,
    }),
  );

  // The caller's experiments (those they made or have an analysis in), newest activity first. The
  // row counts and lists the open (unarchived) ones, every one of them so the count is true, and
  // the first five; the archived ones are read too, only so an empty row can say they exist. Read
  // and shown apart from the analyses, so either read failing leaves the other standing.
  let experiments = $state<Experiment[]>([]);
  let archivedExperiments = $state(0);
  let experimentsLoaded = $state(false);
  /** The first read failed: nothing to show. */
  let experimentsError = $state<string | null>(null);
  /** A later read failed: what is shown may be out of date. */
  let experimentsStale = $state<string | null>(null);
  $effect(() =>
    startPoll({
      fetch: () => listExperiments({ mine: true, archived: true }),
      onData: (l) => {
        experiments = l.experiments.filter((e) => e.archived_at == null);
        archivedExperiments = l.experiments.length - experiments.length;
        experimentsLoaded = true;
        experimentsError = null;
        experimentsStale = null;
      },
      onError: (m) => (experimentsError = m),
      onRefreshError: (m) => (experimentsStale = m),
      intervalMs: 30_000,
      nudge: experimentsNudge,
    }),
  );

  // Open or closed, per row: as the viewer left it in this browser (with the count it had), else
  // the row's default.
  let choices = $state<Record<AccordionRow, RowChoice | null>>({
    attention: rememberedChoice('attention'),
    running: rememberedChoice('running'),
    finished: rememberedChoice('finished'),
    experiments: rememberedChoice('experiments'),
  });
  const totalOf = (key: AccordionRow) =>
    key === 'experiments' ? experiments.length : rows[key].total;
  const isOpen = (key: AccordionRow) => rowOpen(key, choices[key], totalOf(key));
  function toggle(key: AccordionRow) {
    const choice = { open: !isOpen(key), seen: totalOf(key) };
    choices[key] = choice;
    rememberChoice(key, choice);
  }

  // The pins and the pipeline list the Pinned pipelines section (PinnedSection) reads, loaded with
  // the page.
  const catalog = new WorkflowCatalog();
  $effect(() => {
    void loadPins();
    catalog.load();
  });
  const empty = $derived(!loading && !error && !started);
  // Beside the welcome (no analysis yet), the Experiments row shows only when it has something to
  // say: experiments the caller made (or is in), or that they could not be read.
  const experimentsBesideWelcome = $derived(
    !!experimentsError || experiments.length > 0 || archivedExperiments > 0,
  );
  const summary = $derived(workSummary(rows.running.total, rows.attention.total));

  // Dismissing from Needs attention: never navigates, and keeps focus in the row (on the section's
  // heading once the rows it was on are gone).
  function focusAttention() {
    document.getElementById('attention-toggle')?.focus();
  }
  /** Once the server has a dismissal: list the next ones, and refresh the experiments' flags. */
  function refreshAfterDismissal() {
    analysesNudge.now();
    experimentsNudge.now();
  }
  function dismissOne(run: RunSummary) {
    focusAttention();
    void dismissals.dismiss([run.run_id], runName(run), refreshAfterDismissal);
  }
  /** Dismiss exactly the rows on screen (`Dismiss these 5`, or `Dismiss all 3` when that is all). */
  function dismissShown() {
    const runs = rows.attention.runs.slice(0, ROW_LIMIT);
    focusAttention();
    const what = runs.length === 1 ? runName(runs[0]) : `${runs.length} analyses`;
    void dismissals.dismiss(
      runs.map((r) => r.run_id),
      what,
      refreshAfterDismissal,
    );
  }
</script>

<section class="hero">
  <div>
    <h1>My work</h1>
    {#if summary}<p class="lede">Your analyses: {summary}</p>{/if}
  </div>
  <a class="start" href="/runs/new" data-tour={TOUR_ANCHOR.start}>Start an analysis →</a>
</section>

<!-- One accordion row of My work: its heading is the toggle; `count` follows the title. -->
{#snippet accordion(
  key: AccordionRow,
  title: string,
  count: Snippet,
  body: Snippet,
  tour: string | undefined = undefined,
)}
  {@const open = isOpen(key)}
  <section class="work-row" aria-labelledby={`${key}-h`} data-tour={tour}>
    <h2 id={`${key}-h`}>
      <button
        id={`${key}-toggle`}
        type="button"
        class="acc"
        aria-expanded={open}
        aria-controls={`${key}-body`}
        onclick={() => toggle(key)}
      >
        <span class="chev" class:open aria-hidden="true">▸</span>
        {title}
        {@render count()}
      </button>
    </h2>
    <div id={`${key}-body`} class="acc-body" hidden={!open}>{@render body()}</div>
  </section>
{/snippet}

{#snippet experimentsCount()}
  {#if experimentsError}
    <span class="warn">couldn't load</span>
  {:else if !experimentsLoaded}
    <span class="count" aria-hidden="true">…</span><span class="sr-only">loading</span>
  {:else}
    <span class="count">{experiments.length}</span>
    {#if experimentsStale}<span class="warn" title={experimentsStale}>may be out of date</span>{/if}
  {/if}
{/snippet}

{#snippet experimentsBody()}
  {#if experimentsError}
    <p class="error">Couldn't load your experiments: {experimentsError}</p>
  {:else if !experimentsLoaded}
    <p class="muted">Loading…</p>
  {:else if experiments.length}
    <ul>
      {#each experiments.slice(0, ROW_LIMIT) as experiment (experiment.id)}
        <ExperimentRow {experiment} />
      {/each}
    </ul>
    {#if experiments.length > ROW_LIMIT}
      <a class="all" href={EXPERIMENTS_SEE_ALL}>See all {experiments.length} →</a>
    {/if}
  {:else if archivedExperiments}
    <p class="muted" data-testid="my-experiments-empty">
      No open experiments of yours ({archivedExperiments} archived).
      <a href={EXPERIMENTS_ARCHIVED}>See your archived experiments →</a>
    </p>
  {:else}
    <p class="muted" data-testid="my-experiments-empty">
      None of your analyses is in an experiment yet.
      <a href="/experiments">Group analyses under an experiment →</a>
    </p>
  {/if}
  {#if experimentsStale}
    <p class="muted stale-note">
      Couldn't refresh your experiments ({experimentsStale}); showing the last ones read.
    </p>
  {/if}
{/snippet}

{#if loading}
  <p class="muted">Loading…</p>
{:else if error}
  <p class="error" role="alert">Couldn't load your analyses: {error}</p>
{:else if empty}
  <Welcome />
{/if}
{#if !loading && !error && !empty}
  {#if analysesStale}
    <p class="muted stale-note" role="status">
      Couldn't refresh your analyses ({analysesStale}); showing the last ones read.
    </p>
  {/if}
  <div class="work">
    {#each WORK_ROWS as row (row.key)}
      {@const { runs, total } = rows[row.key]}
      {#snippet runCount()}<span class="count">{total}</span>{/snippet}
      {@const attention = row.key === 'attention'}
      {@const listed = runs.slice(0, ROW_LIMIT)}
      {#snippet runBody()}
        {#if attention && dismissals.error}
          <p class="error" role="alert">Couldn't dismiss: {dismissals.error}</p>
        {/if}
        {#if runs.length}
          {#if attention}
            <div class="row-actions">
              <button
                type="button"
                class="link"
                data-testid="dismiss-all"
                aria-label={`${dismissAllLabel(listed.length, total)} that need attention`}
                onclick={dismissShown}>{dismissAllLabel(listed.length, total)}</button
              >
            </div>
          {/if}
          <ul>
            {#each listed as run (run.run_id)}
              <RunRow {run} onDismiss={attention ? () => dismissOne(run) : undefined} />
            {/each}
          </ul>
          {#if total > listed.length}
            <a class="all" href={seeAllHref(row)}>See all {total} →</a>
          {/if}
        {:else}
          <p class="muted">
            {row.empty}
            {#if row.key === 'running'}<a href="/runs/new">Start an analysis →</a>{/if}
          </p>
        {/if}
      {/snippet}
      {@render accordion(
        row.key,
        row.title,
        runCount,
        runBody,
        row.key === 'running' ? TOUR_ANCHOR.running : undefined,
      )}
    {/each}
    {@render accordion('experiments', 'Experiments', experimentsCount, experimentsBody)}
  </div>
  <p class="sr-only" aria-live="polite">{dismissals.announcement}</p>
{:else if !empty || experimentsBesideWelcome}
  <!-- Apart from the analyses: while they load, when they fail, and beside the welcome. -->
  <div class="work">
    {@render accordion('experiments', 'Experiments', experimentsCount, experimentsBody)}
  </div>
{/if}

{#if !loading}
  <PinnedSection {catalog} />
{/if}

<style>
  .hero {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
    margin-bottom: 1.25rem;
  }
  h1 {
    margin: 0;
  }
  .lede {
    color: var(--text-muted);
    margin: 0.3rem 0 0;
  }
  .start {
    padding: 0.55rem 1.1rem;
    border-radius: 0.5rem;
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    text-decoration: none;
    font-weight: 600;
    white-space: nowrap;
  }
  .start:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  section {
    max-width: 1100px;
    margin-bottom: 1.5rem;
  }
  h2 {
    font-size: 1rem;
    margin: 0 0 0.6rem;
  }
  .count {
    font-weight: 400;
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  /* An accordion row: its heading is the toggle. */
  .work {
    max-width: 1100px;
    margin-bottom: 1.5rem;
    border-bottom: 1px solid var(--border);
  }
  .work-row {
    border-top: 1px solid var(--border);
    margin-bottom: 0;
    padding: 0.2rem 0;
  }
  .work-row h2 {
    margin: 0;
  }
  .acc {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    width: 100%;
    padding: 0.55rem 0.25rem;
    border: none;
    background: none;
    color: var(--text);
    font: inherit;
    font-weight: 600;
    text-align: left;
    cursor: pointer;
  }
  .acc:hover {
    background: var(--hover);
  }
  .acc:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .chev {
    display: inline-block;
    width: 0.8rem;
    color: var(--text-muted);
    transition: transform 0.12s ease-out;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  @media (prefers-reduced-motion: reduce) {
    .chev {
      transition: none;
    }
  }
  .acc-body {
    padding: 0.2rem 0 0.8rem;
  }
  .acc-body .muted {
    margin: 0.2rem 0.25rem;
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0;
    padding: 0;
  }
  .all {
    display: inline-block;
    margin-top: 0.5rem;
    font-size: 0.82rem;
    color: var(--accent);
  }
  /* A link inside running text is underlined, not told apart by colour alone. */
  .muted a {
    color: var(--accent);
    text-decoration: underline;
  }
  /* The Experiments row's header note when its read failed: visible with the row closed. */
  .warn {
    font-weight: 400;
    font-size: 0.8rem;
    color: var(--fg-fail);
  }
  .row-actions {
    display: flex;
    justify-content: flex-end;
    margin: 0 0.25rem 0.4rem;
    font-size: 0.82rem;
  }
  .stale-note {
    font-size: 0.8rem;
  }
</style>
