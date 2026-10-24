<script lang="ts">
  import type { Snippet } from 'svelte';
  import { page } from '$app/state';
  import { getWorkflow, listWorkflows } from '#lib/api/index.js';
  import { startPoll } from '#lib/util/poll.js';
  import { errorMessage } from '#lib/util/format.js';
  import { groupVersions, workflowKey } from '#lib/workflows/version.js';
  import { WorkflowState, setWorkflowContext } from '#lib/workflows/workflowContext.svelte.js';
  import { workflowTabs, workflowTabHref, activeWorkflowTab } from '#lib/workflows/workflowNav.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { LAB_WORKFLOW_TABS } from '#lib/view/viewRoutes.js';
  import ExpertHint from '#lib/shell/ExpertHint.svelte';
  import VersionHeader from '#lib/workflows/VersionHeader.svelte';
  import ErrorPage from '#lib/components/ErrorPage.svelte';
  import { errorKind } from '#lib/components/errorPage.js';

  // One workflow version: loads it and its sibling versions, and renders the header and the tabs
  // (Run, About, Parameters, Versions, Provenance), each its own route.
  let { children }: { children: Snippet } = $props();

  const wf = setWorkflowContext(new WorkflowState());
  const id = $derived(page.params.id ?? '');
  const tab = $derived(activeWorkflowTab(page.url.pathname));

  // Fetch the requested frozen version. Guard every continuation with `cancelled` so a rapid version
  // switch (the effect re-runs on `id` change) can't have an earlier request resolve last and
  // overwrite the newer version's detail — or, worse, its late .catch flip the UI to an error screen
  // over already-correct data.
  $effect(() => {
    const wid = id;
    // Try again on the error page loads it afresh.
    void wf.loadAttempt;
    if (!wid) return;
    wf.loading = true;
    wf.error = null;
    wf.detail = null;
    // While the version is being built (its engine runtime or images), keep it fresh so the page
    // turns runnable by itself; stop once it is ready or failed.
    // A version that is not there (or not this user's) is not asked again.
    let stop = () => {};
    stop = startPoll({
      fetch: () => getWorkflow(wid),
      // A later poll that succeeds after a failed first load replaces the error screen.
      onData: (d) => {
        wf.detail = d;
        wf.error = null;
      },
      onError: (m, e) => {
        wf.error = m;
        wf.errorKind = errorKind(e);
        // Stopping cancels the poll, `onSettled` included: the spinner is cleared here.
        if (wf.errorKind === 'not-found' || wf.errorKind === 'forbidden') {
          wf.loading = false;
          stop();
        }
      },
      onSettled: () => (wf.loading = false),
      until: (d) => d.state !== 'pending',
      intervalMs: 10_000,
    });
    return () => stop();
  });

  // The sibling versions share the version's (project, name). The effect reads only these two
  // strings, not `wf.detail`: the poll above replaces `detail` every 10 s while the version builds,
  // and the whole registry list must not be fetched again on each tick. A version switch clears
  // `detail` and sets it again, so the list is fetched once per switch. Same out-of-order guard.
  const project = $derived(wf.detail?.project);
  const name = $derived(wf.detail?.name);
  $effect(() => {
    const p = project;
    const n = name;
    // Another workflow's versions must not stay in the picker while this one's load.
    wf.versions = [];
    wf.versionsLoading = true;
    wf.versionsError = null;
    if (p === undefined || n === undefined) return;
    let cancelled = false;
    listWorkflows()
      .then((all) => {
        // The same grouping (and newest-first order) the Workflows list and New run use.
        const key = workflowKey({ project: p, name: n });
        if (!cancelled) wf.versions = groupVersions(all).find((g) => g.key === key)?.versions ?? [];
      })
      .catch((e) => {
        if (!cancelled) wf.versionsError = errorMessage(e);
      })
      .finally(() => {
        if (!cancelled) wf.versionsLoading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  // The Lab view shows what the pipeline is for and how to start it (Run, About); its versions,
  // parameters and provenance are Expert tabs.
  const lab = $derived(view.mode === 'lab');
</script>

<a class="back-link" href="/workflows">← {lab ? 'Pipelines' : 'Workflows'}</a>

{#if wf.loading}
  <p class="muted">Loading…</p>
{:else if wf.error}
  <ErrorPage
    kind={wf.errorKind}
    subject="workflow"
    detail={wf.error}
    onretry={() => wf.loadAttempt++}
  />
{:else if wf.detail}
  {@const detail = wf.detail}
  <VersionHeader {wf} {detail} {id} {tab} {lab} />

  <nav class="tabs" aria-label="Workflow views">
    {#each workflowTabs(detail).filter((t) => !lab || LAB_WORKFLOW_TABS.includes(t)) as t}
      <a
        href={workflowTabHref(id, t)}
        class:active={tab === t}
        aria-current={tab === t ? 'page' : undefined}
        data-sveltekit-reset="false">{t}</a
      >
    {/each}
  </nav>

  {@render children()}
  <ExpertHint what="Looking for its versions, parameters and provenance?" />
{/if}

<style>
  nav.tabs {
    display: flex;
    gap: 0.25rem;
    margin: 1rem 0 1rem;
    border-bottom: 1px solid var(--border);
  }
  nav.tabs a {
    padding: 0.4rem 0.8rem;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    color: var(--text-muted);
    text-decoration: none;
    text-transform: capitalize;
    font-size: 0.85rem;
  }
  nav.tabs a:hover {
    color: var(--text);
  }
  nav.tabs a.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .muted {
    font-size: 0.85rem;
  }
</style>
