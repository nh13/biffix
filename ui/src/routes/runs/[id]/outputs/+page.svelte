<script lang="ts">
  import { goto } from '$app/navigation';
  import { outputsViewHref, parseOutputsView, type OutputsView } from '$lib/runs/outputsUrl';
  import { jobsHref } from '$lib/runs/jobsUrl';
  import { page } from '$app/state';
  import { replaceUrl } from '$lib/util/replaceUrl';
  import { isLiveRun } from '$lib/runs/runState';
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import { viewParams } from '$lib/util/urlParam';
  import RunOutputs from '$lib/runs/RunOutputs.svelte';
  import { runOutputsTotal } from '$lib/runs/runNav';

  const ctx = getRunContext();

  // The view, filters and open file live in the URL, so any of them can be linked; the search, task
  // and file can name a sample or a file, so they are in the fragment (never sent to a server).
  const view = $derived(parseOutputsView(viewParams(page.url)));

  function setView(next: OutputsView) {
    const url = page.url;
    const href = outputsViewHref(url.pathname, next, viewParams(url));
    // Opening or closing a file is a step Back undoes; filter tweaks replace the entry.
    if (next.file !== view.file) goto(href, { keepFocus: true, noScroll: true });
    else replaceUrl(href);
  }
</script>

{#if ctx.run}
  {@const run = ctx.run}
  <RunOutputs
    runId={run.run_id}
    project={run.project}
    engine={run.engine}
    outputsTotal={runOutputsTotal(run, ctx.outputsTotal)}
    live={isLiveRun(run)}
    task={view.task}
    onclearTask={() => setView({ ...view, task: null })}
    urlState={view}
    onstate={setView}
    onjumpTask={(key) => goto(jobsHref(run.run_id, { job: key }))}
  />
{/if}
