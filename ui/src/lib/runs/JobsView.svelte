<script lang="ts">
  import { untrack } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import {
    listTasks,
    type RunDetail,
    type TaskPage,
    type TaskSort,
    type TaskStateFilter,
  } from '$lib/api';
  import { debounce } from '$lib/util/debounce';
  import { errorMessage } from '$lib/util/format';
  import { replaceUrl } from '$lib/util/replaceUrl';
  import { startPoll } from '$lib/util/poll';
  import { isLiveRun } from '$lib/runs/runState';
  import { UrlEcho } from '$lib/util/urlEcho';
  import { viewParams } from '$lib/util/urlParam';
  import { jobsViewHref, parseJobsView, STATE_FILTERS, type JobsView } from '$lib/runs/jobsUrl';
  import { runTabHref } from '$lib/runs/runNav';
  import { fetchWindow, newestOnly } from '$lib/runs/taskWindow';
  import { nextJobIndex } from '$lib/runs/jobsTable';
  import { focusRadio, radioKeyTarget } from '$lib/util/radioGroup';
  import { overlayOpen, shortcutBlocked } from '$lib/runs/analysis/analysisNav';
  import JobPane from '$lib/runs/JobPane.svelte';
  import JobsTable from '$lib/runs/JobsTable.svelte';

  type Task = TaskPage['tasks'][number];

  // The run's jobs, a window at a time from the server. Filters, search and sort live in the URL
  // fragment, and selecting a job opens its detail pane (also in the fragment, so it can be linked
  // without its task key, which names a sample, reaching a server).
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  const PAGE = 200;
  const POLL_MS = 5000;
  const SORT_LABELS: Record<TaskSort, string> = {
    key: 'Task',
    state: 'State',
    duration: 'Duration',
    started: 'Started',
  };

  // Primitives read off the run: the layout replaces `run` on every poll, and an effect that read
  // the object itself would re-run (restarting the table's poll) each time.
  const runId = $derived(run.run_id);
  const live = $derived(isLiveRun(run));
  const groups = $derived((run.dag?.nodes ?? []).map((n) => n.id).sort());

  const view = $derived(parseJobsView(viewParams(page.url)));

  let rows = $state<Task[]>([]);
  let total = $state(0);
  let counts = $state<TaskPage['counts'] | null>(null);
  let logGroup = $state<string | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  /** How many rows the table holds (grows with "Show more"; every refresh re-fetches this many). */
  let loaded = $state(PAGE);

  // The filters alone, as a string: a derived string only changes when its value does, so selecting
  // a job (also in the URL) does not reload the table.
  const filters = $derived(
    JSON.stringify({
      state: view.state,
      group: view.group,
      q: view.q.trim(),
      sort: view.sort,
      descending: view.descending,
    }),
  );
  const filterView = $derived<JobsView>({ ...JSON.parse(filters), job: null });

  // The table's first `limit` rows under `v`, always fetched whole (never an offset page appended to
  // rows read earlier, which a live, re-sorting list shifts under), and null when a newer request
  // for the table (new filters, "Show more", a later refresh) was made meanwhile. So the table never
  // repeats a row, mixes two filters' rows, or shrinks back to an older, shorter window.
  const fetchRows = newestOnly((id: string, v: JobsView, limit: number) =>
    fetchWindow(
      (offset, size) =>
        listTasks(id, {
          state: v.state,
          group: v.group,
          q: v.q || null,
          sort: v.sort,
          descending: v.descending,
          offset,
          limit: size,
        }),
      limit,
    ),
  );
  function apply(p: TaskPage | null) {
    if (!p) return; // superseded by a newer request
    rows = p.tasks;
    total = p.total;
    counts = p.counts;
    logGroup = p.log_group ?? null;
    error = null;
  }

  // (Re)load when the filters change, then keep the loaded window fresh while the run is live. Only
  // the filters and the run id are dependencies: liveness is read as each refresh lands, so the run
  // ending stops the poll without throwing away the rows "Show more" loaded.
  $effect(() => {
    const id = runId;
    const v = filterView;
    return untrack(() => {
      // The last filter's rows are not this one's: show the spinner (or this filter's error)
      // until its own rows land.
      loaded = PAGE;
      loading = true;
      rows = [];
      total = 0;
      counts = null;
      error = null;
      return startPoll({
        fetch: () => fetchRows(id, v, loaded),
        onData: apply,
        onError: (m) => (error = m),
        onSettled: () => (loading = false),
        until: () => !live,
        intervalMs: POLL_MS,
      });
    });
  });

  let loadingMore = $state(false);
  async function showMore() {
    const before = loaded;
    loaded = rows.length + PAGE;
    loadingMore = true;
    try {
      apply(await fetchRows(runId, filterView, loaded));
    } catch (e) {
      // The table keeps the rows it has, and the poll keeps refreshing that many.
      loaded = before;
      error = errorMessage(e);
    } finally {
      loadingMore = false;
    }
  }

  /** The current URL showing `v` (`jobsViewHref`). */
  const viewHref = (v: JobsView): string => jobsViewHref(page.url, v);

  function setView(patch: Partial<JobsView>) {
    return replaceUrl(viewHref({ ...view, ...patch }));
  }

  // Selecting a job pushes history, so Back closes the pane again.
  function select(taskKey: string | null) {
    return goto(viewHref({ ...view, job: taskKey }), { keepFocus: true, noScroll: true });
  }

  // The search box updates the URL once typing pauses, not on every keystroke. The URL's `q` comes
  // back trimmed, a round trip later: that echo of a report is not copied into the box (it would
  // drop a trailing space, or keystrokes typed meanwhile); any other change (back/forward, a link)
  // is.
  let search = $state('');
  const urlQ = $derived(view.q);
  const echo = new UrlEcho();
  $effect(() => {
    const q = urlQ;
    if (!echo.isEcho(q)) search = q;
  });
  const onsearch = debounce(() => {
    echo.reported(search.trim(), view.q);
    setView({ q: search });
  }, 250);
  $effect(() => onsearch.cancel);

  // The selected job: from the loaded rows, else fetched (a link to a job off this page) and kept
  // fresh while the run is live — but only the job the URL names, never the one fetched for the
  // previous selection.
  const jobKey = $derived(view.job);
  const listedJob = $derived(jobKey ? (rows.find((t) => t.task_key === jobKey) ?? null) : null);
  const jobListed = $derived(listedJob !== null);
  let fetchedJob = $state<Task | null>(null);
  // The last listed copy stands in while a job that just left the rows (it no longer matches the
  // filter) is fetched, so its pane is not closed and reopened.
  $effect(() => {
    if (listedJob) fetchedJob = listedJob;
  });
  const selected = $derived(
    listedJob ?? (fetchedJob && fetchedJob.task_key === jobKey ? fetchedJob : null),
  );
  $effect(() => {
    const key = jobKey;
    const id = runId;
    if (!key || jobListed) return;
    // By exact key: a substring search could bury it under the many keys that contain it. A failed
    // refresh keeps the job shown; only a failed first look closes it.
    return startPoll({
      fetch: () => listTasks(id, { taskKey: key, limit: 1 }),
      onData: (p) => (fetchedJob = p.tasks.find((t) => t.task_key === key) ?? null),
      onError: () => (fetchedJob = null),
      until: () => !live,
      intervalMs: POLL_MS,
    });
  });

  // Keyboard: j/k move the selection down/up the loaded rows (opening the pane), Esc closes it.
  // Ignored while typing in a field, or behind (or inside) a dialog or the open nav drawer, as the
  // Lab's previous / next keys are (`shortcutBlocked`).
  function onkeydown(e: KeyboardEvent) {
    if (shortcutBlocked(e, overlayOpen())) return;
    if (e.key === 'Escape' && view.job) {
      e.preventDefault();
      select(null);
    } else if ((e.key === 'j' || e.key === 'k') && rows.length > 0) {
      e.preventDefault();
      const at = rows.findIndex((t) => t.task_key === view.job);
      const next = nextJobIndex(at, e.key === 'j' ? 1 : -1, rows.length);
      replaceUrl(viewHref({ ...view, job: rows[next].task_key }));
      document.getElementById(`job-row-${next}`)?.scrollIntoView({ block: 'nearest' });
    }
  }

  /** The state filters are one radio group: one Tab stop, the arrow keys (and Home / End) move. */
  function stateKeydown(e: KeyboardEvent) {
    const to = radioKeyTarget(e.key, STATE_FILTERS.indexOf(view.state), STATE_FILTERS.length);
    if (to === null) return;
    e.preventDefault();
    setView({ state: STATE_FILTERS[to] });
    focusRadio(e, to);
  }

  function chipLabel(filter: TaskStateFilter): string {
    const n = counts?.[filter];
    return n == null ? filter : `${filter} ${n}`;
  }
</script>

<svelte:window {onkeydown} />

<div class="toolbar">
  <div class="tabs chips" role="radiogroup" aria-label="Jobs by state">
    {#each STATE_FILTERS as f}
      <button
        role="radio"
        aria-checked={view.state === f}
        tabindex={view.state === f ? 0 : -1}
        class:active={view.state === f}
        onclick={() => setView({ state: f })}
        onkeydown={stateKeydown}>{chipLabel(f)}</button
      >
    {/each}
  </div>
  {#if groups.length > 0}
    <select
      aria-label="Step"
      value={view.group ?? ''}
      onchange={(e) => setView({ group: e.currentTarget.value || null })}
    >
      <option value="">All steps</option>
      {#each groups as g}<option value={g}>{g}</option>{/each}
    </select>
  {/if}
  <input
    class="search"
    type="search"
    placeholder="Search task keys"
    aria-label="Search task keys"
    bind:value={search}
    oninput={onsearch}
  />
  <label class="sort">
    Sort
    <select
      value={view.sort}
      onchange={(e) => setView({ sort: e.currentTarget.value as TaskSort })}
    >
      {#each Object.entries(SORT_LABELS) as [value, label]}<option {value}>{label}</option>{/each}
    </select>
  </label>
  <button
    class="order"
    aria-label={view.descending
      ? 'Descending; switch to ascending'
      : 'Ascending; switch to descending'}
    onclick={() => setView({ descending: !view.descending })}>{view.descending ? '↓' : '↑'}</button
  >
</div>

{#if view.group}
  <p class="muted note">
    Step <strong>{view.group}</strong> —
    <button class="linkish" onclick={() => setView({ group: null })}>show all steps</button>
  </p>
{/if}

<div class="layout" class:with-pane={selected}>
  <div class="list">
    {#if loading && rows.length === 0}
      <p class="muted">Loading jobs…</p>
    {:else if error && rows.length === 0}
      <p class="error">Couldn't load jobs: {error}</p>
    {:else if (run.tasks_observed ?? 0) === 0 && total === 0}
      <p class="muted">
        No jobs were scheduled for this run.
        {#if run.state === 'failed'}
          It failed in the coordinator before submitting any child jobs; see the
          <a href={runTabHref(runId, 'coordinator')}>coordinator log</a>.
        {/if}
      </p>
    {:else if rows.length === 0}
      <p class="muted">No jobs match these filters.</p>
    {:else}
      {#if error}<p class="error" role="alert">Couldn't load jobs: {error}</p>{/if}
      <JobsTable
        {rows}
        {total}
        selected={view.job}
        onselect={select}
        page={PAGE}
        {loadingMore}
        onshowmore={showMore}
      />
    {/if}
  </div>
  {#if selected}
    <div class="pane-slot">
      <JobPane {run} task={selected} {logGroup} onselect={select} onclose={() => select(null)} />
    </div>
  {/if}
</div>

<style>
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem;
    margin-bottom: 0.75rem;
    font-size: 0.8rem;
  }
  .chips {
    margin: 0;
  }
  .chips button {
    text-transform: capitalize;
  }
  select,
  .search {
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: inherit;
    font: inherit;
  }
  .search {
    flex: 1 1 12rem;
    max-width: 20rem;
  }
  .sort {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    color: var(--text-muted);
  }
  .order {
    padding: 0.2rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: inherit;
    cursor: pointer;
  }
  .note {
    font-size: 0.8rem;
    margin: 0 0 0.5rem;
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 1rem;
    align-items: start;
  }
  .layout.with-pane {
    grid-template-columns: minmax(0, 1.4fr) minmax(0, 1fr);
  }
  .pane-slot {
    /* Below the pinned run tab bar. */
    position: sticky;
    top: 3.25rem;
    max-height: calc(100vh - 4rem);
    overflow: auto;
  }
  .list {
    overflow-x: auto;
  }
  @media (max-width: 1000px) {
    .layout.with-pane {
      grid-template-columns: minmax(0, 1fr);
    }
    .pane-slot {
      position: static;
      max-height: none;
    }
  }
  .linkish {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
    font: inherit;
  }
</style>
