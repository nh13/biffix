<script lang="ts">
  import type { Snippet } from 'svelte';
  import { page } from '$app/state';
  import { getRun, listOutputs, listRunReports } from '#lib/api/index.js';
  import { startPoll } from '#lib/util/poll.js';
  import { isTerminalRun } from '#lib/runs/runState.js';
  import { RunState, setRunContext } from '#lib/runs/runContext.svelte.js';
  import RunHeader from '#lib/runs/RunHeader.svelte';
  import RunTabs from '#lib/runs/RunTabs.svelte';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { markRead } from '#lib/notifications/notifications.svelte.js';
  import { onStatusProbe } from '#lib/status/live.svelte.js';
  import { resultCards } from '#lib/runs/results.js';
  import ErrorPage from '#lib/components/ErrorPage.svelte';
  import { errorKind } from '#lib/components/errorPage.js';
  import { errorMessage } from '#lib/util/format.js';

  // Every view of one run: polls the run (until it ends) and shares it with the views below, under
  // a header that stays the same across them.
  let { children }: { children: Snippet } = $props();

  const runState = setRunContext(new RunState());

  // Whether the full header is on screen; once it scrolls away, the pinned tab bar carries the
  // run's identity and actions instead.
  let headerEl = $state<HTMLElement | null>(null);
  let headerVisible = $state(true);
  $effect(() => {
    if (!headerEl) return;
    const observer = new IntersectionObserver(([entry]) => {
      headerVisible = entry.isIntersecting;
    });
    observer.observe(headerEl);
    return () => observer.disconnect();
  });

  // The Lab view's analysis page (the run's own URL) and its file list are pages with their own
  // header; the run's other pages are Expert pages, with the run header and tabs in either view.
  const labPage = $derived(
    (view.mode === 'lab' && page.route.id === '/runs/[id]') || page.route.id === '/runs/[id]/files',
  );

  // A run keeps changing until it reaches a terminal state; poll live until then.
  const POLL_MS = 4000;

  // The run's id alone, not `page.url`: a tab's query-string state (a filter, an open file) changes
  // the URL too, and re-running the poll on it re-showed "Loading…" and remounted the tab.
  const runId = $derived(page.params.id);
  $effect(() => {
    const id = runId;
    void runState.loadAttempt;
    if (!id) return;
    // Another run (the layout is reused when only the id changes, e.g. the "resumed from" link):
    // drop everything of the previous one, so no view shows or links its data under this id.
    // Clearing `run` also clears `reportsKey`, which cancels a reports load still in flight.
    runState.loading = true;
    runState.error = null;
    runState.run = null;
    runState.reports = null;
    runState.reportsError = null;
    runState.stale = null;
    runState.actions.reset();
    // The first paint drives the spinner + surfaced error; background refreshes keep the last good
    // view on a transient failure, and say it may be out of date. `until` stops polling once the
    // run reaches a terminal state; a run that is not there (or not this user's) is not asked
    // again, while a server or network failure is retried on the next beat.
    let stop = () => {};
    stop = startPoll({
      // The views page tasks themselves (POST /runs/{id}/tasks/query), so the poll leaves them out.
      fetch: () => getRun(id, { tasks: false }),
      onData: (r) => {
        runState.run = runState.withRename(r);
        runState.error = null;
        runState.stale = null;
      },
      onError: (m, e) => {
        runState.error = m;
        runState.errorKind = errorKind(e);
        // Stopping cancels the poll, `onSettled` included: the spinner is cleared here.
        if (runState.errorKind === 'not-found' || runState.errorKind === 'forbidden') {
          runState.loading = false;
          stop();
        }
      },
      onRefreshError: (m) => (runState.stale = m),
      onSettled: () => (runState.loading = false),
      until: (r) => isTerminalRun(r),
      intervalMs: POLL_MS,
    });
    return () => stop();
  });

  // Opening an ended run, in either view and on any of its pages, counts as seeing it: its
  // notifications are marked read (it leaves the bell's unread, but stays in My work's Needs attention
  // until dismissed), once
  // per run each time the page is opened, and once when it ends while the page is open. (Opening it
  // from the bell sends no read of its own: this is the one.) A read the server refuses is tried
  // again on the status poll's next beat, so at most once per beat.
  let markedRead: string | null = null;
  let reading: string | null = null;
  let readAttempt = $state(0);
  const endedRunId = $derived(
    runState.run && isTerminalRun(runState.run) ? runState.run.run_id : null,
  );
  $effect(() => {
    const id = endedRunId;
    void readAttempt;
    if (!id || id === markedRead || id === reading) return;
    reading = id;
    void markRead({ run_ids: [id] }).then((taken) => {
      if (reading === id) reading = null;
      if (taken) markedRead = id;
    });
  });
  $effect(() =>
    onStatusProbe(() => {
      if (endedRunId && endedRunId !== markedRead && endedRunId !== reading) readAttempt++;
    }),
  );

  // A resumed run's output count with the files it reused (`RunState.outputsTotal`): the total of a
  // one-row page of its outputs list, re-asked only when its outputs could have changed. A run that
  // resumed nothing needs no request: its `outputs_total` is the count.
  const outputsKey = $derived(
    runState.run?.resumed_from
      ? `${runState.run.run_id}:${runState.run.outputs_total ?? 0}:${isTerminalRun(runState.run)}`
      : null,
  );
  // The shown run's id: the count and the reports below follow it, not every poll's new `run`.
  const shownRunId = $derived(runState.run?.run_id);
  // The run the count is of: another run's is dropped at once, not shown until its own loads.
  let countedRunId: string | undefined;
  $effect(() => {
    const key = outputsKey;
    const id = shownRunId;
    if (!key || id !== countedRunId) runState.outputsTotal = null;
    countedRunId = id;
    if (!key || !id) return;
    let cancelled = false;
    listOutputs(id, { limit: 1 })
      .then((listing) => {
        if (!cancelled) runState.outputsTotal = listing.total;
      })
      // Without it the tab shows the run's own count, as before.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  // The run's reports, re-asked only when its outputs could have changed (not on every poll), or
  // when a view asks again after a failure (`reportsAttempt`).
  const reportsKey = $derived(
    runState.run
      ? `${runState.run.run_id}:${runState.run.outputs_total ?? 0}:${isTerminalRun(runState.run)}:${runState.reportsAttempt}`
      : null,
  );
  $effect(() => {
    const key = reportsKey;
    const id = shownRunId;
    if (!key || !id) return;
    let cancelled = false;
    runState.reportsError = null;
    listRunReports(id)
      .then((r) => {
        if (cancelled) return;
        runState.reports = r.reports;
        runState.reportsError = null;
      })
      .catch((e) => {
        // A failure is not "still loading": keep the last good list (or none) and say why, so the
        // Reports view can offer a retry instead of a spinner that never ends.
        if (!cancelled) runState.reportsError = errorMessage(e);
      });
    return () => {
      cancelled = true;
    };
  });
</script>

<div class="run-page">
  {#if runState.loading}
    <p class="muted">Loading…</p>
  {:else if runState.error}
    <ErrorPage
      kind={runState.errorKind}
      subject="run"
      detail={runState.error}
      onretry={() => runState.loadAttempt++}
    />
  {:else if runState.run}
    {#if runState.stale}<p class="muted stale" role="status">
        Couldn't check for updates; this page may be out of date.
      </p>{/if}
    {#if !labPage}
      <div bind:this={headerEl}><RunHeader run={runState.run} /></div>
      <RunTabs
        run={runState.run}
        reports={runState.reports ? resultCards(runState.reports).length : null}
        outputs={runState.outputsTotal}
        compact={!headerVisible}
      />
    {/if}
    {@render children()}
  {/if}
</div>
