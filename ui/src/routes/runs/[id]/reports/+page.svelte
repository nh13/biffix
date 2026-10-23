<script lang="ts">
  import { page } from '$app/state';
  import { getQcGeneralStats, type QcGeneralStats as Stats, type RunReport } from '$lib/api';
  import QcGeneralStats from '$lib/runs/QcGeneralStats.svelte';
  import { replaceUrl } from '$lib/util/replaceUrl';
  import { viewParams, withViewParams } from '$lib/util/urlParam';
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import { outputViewerFile, type ViewerFile } from '$lib/components/viewerFile';
  import OutputViewer from '$lib/components/OutputViewer.svelte';
  import { resultCards } from '$lib/runs/results';
  import {
    isServable,
    openKind,
    openTarget,
    sampleLabels,
    unservedText,
    type OpenTarget,
  } from '$lib/qc/open';
  import ResultViewer from '$lib/qc/ResultViewer.svelte';
  import ReusedFrom from '$lib/runs/ReusedFrom.svelte';

  // The run's reports (its workflow's declared ones, then a MultiQC report, then FastQC results found
  // by name): pick one, see it in the page. A per-sample report is one choice with a sample picker.
  // The open report is `#report=<output id>` in the fragment, so it can be linked without the id
  // reaching a server log. A report the page cannot show in place (a FastQC zip, a BAM) opens in
  // its viewer, as in the Lab view.
  const uid = $props.id();
  const ctx = getRunContext();
  const reports = $derived(ctx.reports);
  const cards = $derived(resultCards(reports ?? []));
  const chosenId = $derived(viewParams(page.url).get('report'));

  const chosenCard = $derived(
    cards.find((c) => c.entries.some((e) => e.report.output.id === chosenId)) ?? cards[0] ?? null,
  );
  const chosen = $derived(
    chosenCard?.entries.find((e) => e.report.output.id === chosenId)?.report ??
      chosenCard?.entries[0]?.report ??
      null,
  );
  const sampleOptions = $derived(
    chosenCard?.perSample
      ? sampleLabels(
          chosenCard.entries.map((e) => ({ sample: e.sample, output: e.report.output })),
        ).map((label, i) => ({ label, report: chosenCard.entries[i].report }))
      : [],
  );
  // How the chosen report opens when the page cannot show it in place.
  const opens = $derived(
    chosen && !chosen.output.viewer ? openKind(chosen.format, chosen.output) : null,
  );
  let open = $state<OpenTarget | null>(null);

  // A MultiQC report's General Statistics, natively (none when it has no data files). Keyed on
  // primitives: the layout replaces `ctx.run` on every poll of a live run, and depending on the
  // object would clear and re-fetch the table each time.
  const runId = $derived(ctx.run?.run_id);
  const isMultiqc = $derived(chosen?.kind === 'multiqc');
  let stats = $state<Stats | null>(null);
  $effect(() => {
    const id = runId;
    const multiqc = isMultiqc;
    stats = null;
    if (!id || !multiqc) return;
    let cancelled = false;
    getQcGeneralStats(id)
      .then((s) => {
        if (!cancelled && s.samples.length > 0) stats = s;
      })
      .catch(() => {
        /* no General Statistics: the report alone */
      });
    return () => {
      cancelled = true;
    };
  });

  function choose(r: RunReport) {
    const url = page.url;
    // The fragment alone: `viewParams` would also copy the query's params into it.
    const view = new URLSearchParams(url.hash.slice(1));
    view.set('report', r.output.id);
    downloadError = null;
    replaceUrl(withViewParams(`${url.pathname}${url.search}`, view));
  }

  // A failed download of the open report; cleared when another report or run is shown.
  let downloadError = $state<string | null>(null);
  $effect(() => {
    runId;
    downloadError = null;
  });
  const viewerFile = (runId: string, r: RunReport): ViewerFile =>
    outputViewerFile(runId, r.output, {
      name: r.sample ? `${r.name} · ${r.sample}` : r.name,
      detail: r.description,
      onDownloadError: (m) => (downloadError = m),
    });
</script>

{#if ctx.run}
  {@const run = ctx.run}
  {#if ctx.reportsError}
    <p class="error" role="alert">
      Couldn't load this run's reports: {ctx.reportsError}
      <button class="link" onclick={() => ctx.reloadReports()}>Try again</button>
    </p>
  {/if}
  {#if reports === null}
    {#if !ctx.reportsError}<p class="muted">Loading reports…</p>{/if}
  {:else if reports.length === 0}
    <div class="empty">
      <p>This run has no reports yet.</p>
      <p class="muted">
        Reports are the files a workflow names under <code>reports:</code> in its
        <code>biffix.yaml</code>, any MultiQC report (<code>multiqc_report.html</code>), and any
        FastQC results (<code>_fastqc.zip</code>) among the run's outputs.
      </p>
    </div>
  {:else}
    {#if cards.length > 1}
      <div class="picker" role="group" aria-label="Reports">
        {#each cards as c (c.key)}
          {@const first = c.entries[0].report}
          <button
            type="button"
            aria-pressed={chosenCard?.key === c.key}
            class:active={chosenCard?.key === c.key}
            title={c.description ?? first.output.path}
            onclick={() => choose(first)}>{c.name}</button
          >
        {/each}
      </div>
    {/if}
    {#if sampleOptions.length > 0}
      <div class="sample">
        <label for="{uid}-sample">Sample</label>
        <select
          id="{uid}-sample"
          value={chosen?.output.id ?? ''}
          onchange={(e) => {
            const id = e.currentTarget.value;
            const r = sampleOptions.find((o) => o.report.output.id === id)?.report;
            if (r) choose(r);
          }}
        >
          {#each sampleOptions as o (o.report.output.id)}<option value={o.report.output.id}
              >{o.label}</option
            >{/each}
        </select>
      </div>
    {/if}
    {#if chosen?.output.reused_from}<p class="reused">
        <ReusedFrom from={chosen.output.reused_from} />
      </p>{/if}
    {#if downloadError}<p class="error" role="alert">{downloadError}</p>{/if}
    {#if stats}<QcGeneralStats runId={run.run_id} {stats} />{/if}
    {#if chosen}
      {#if !isServable(chosen.output)}
        <p class="muted">{chosen.name}: {unservedText(chosen.output).toLowerCase()}.</p>
      {:else if chosen.output.viewer}
        <OutputViewer file={viewerFile(run.run_id, chosen)} embedded />
      {:else if opens}
        {@const file = viewerFile(run.run_id, chosen)}
        <p class="muted">
          {file.name}
          <span class="mono">({chosen.output.name})</span>
          <button
            class="btn btn-compact"
            onclick={() => (open = openTarget(opens, run.run_id, chosen.output, file))}
            >{opens === 'alignment' ? 'View alignments' : 'Open FastQC'}</button
          >
          <button class="link" onclick={() => file.download()}>Download it</button>
        </p>
      {:else}
        <p class="muted">
          {chosen.name} can't be shown in the browser.
          <button class="link" onclick={() => viewerFile(run.run_id, chosen).download()}
            >Download it</button
          >.
        </p>
      {/if}
    {/if}
    {#if open}<ResultViewer target={open} onclose={() => (open = null)} />{/if}
  {/if}
{/if}

<style>
  .picker {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
    margin-bottom: 0.75rem;
  }
  .picker button {
    padding: 0.3rem 0.7rem;
    border: 1px solid var(--border);
    border-radius: 1rem;
    background: var(--bg-content);
    color: var(--text);
    font-size: 0.8rem;
    cursor: pointer;
  }
  .picker button.active {
    border-color: var(--accent);
    color: var(--accent);
    font-weight: 600;
  }
  .empty {
    max-width: 40rem;
  }
  .reused {
    margin: 0 0 0.5rem;
  }
  .sample {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
    font-size: 0.85rem;
  }
  .sample select {
    padding: 0.25rem 0.4rem;
    border: 1px solid var(--control-border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
  }
  .btn {
    margin: 0 0.4rem;
  }
  .empty p {
    margin: 0 0 0.5rem;
  }
</style>
