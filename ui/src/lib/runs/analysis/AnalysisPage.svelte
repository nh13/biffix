<script lang="ts">
  // The Lab view's analysis page: one results-first page, no tabs. A pinned header
  // (name, pipeline, who started it, Cancel / Try again), the plain status, then its sections in
  // the order its state calls for: what went wrong, results, QC at a glance, samples, inputs used
  // and cost. The run itself comes from the route's layout, which polls it.
  import { experimentHref } from '$lib/experiments/experiments';
  import {
    getQcFastqcStatus,
    getRun,
    getQcGeneralStats,
    getWorkflow,
    getWorkflowEstimate,
    type FastqcStatusReport,
    type QcGeneralStats as Stats,
    type RunDetail,
    type RunSummary,
    type WorkflowDetail,
    type WorkflowEstimate,
  } from '$lib/api';
  import { currentIdentity } from '$lib/api/auth';
  import { errorMessage } from '$lib/util/format';
  import { page } from '$app/state';
  import FastqcFromMultiqc from '$lib/qc/FastqcFromMultiqc.svelte';
  import QcPreviews from '$lib/qc/QcPreviews.svelte';
  import QcGeneralStats from '$lib/runs/QcGeneralStats.svelte';
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import { FailureSummaryLoader } from '$lib/runs/failureLoader.svelte';
  import { runVersionHref, runVersionLabel } from '$lib/runs/runVersion';
  import { noCostLine } from '$lib/runs/cost';
  import { isLiveRun, isTerminalRun } from '$lib/runs/runState';
  import {
    cancelledBanner,
    costText,
    progressFraction,
    sectionOrder,
    startedLine,
  } from './analysis';
  import AddToExperiment from '$lib/experiments/AddToExperiment.svelte';
  import AnalysisResults from './AnalysisResults.svelte';
  import { qcChoices } from '$lib/qc/open';
  import InputsUsed from './InputsUsed.svelte';
  import SampleGrid from './SampleGrid.svelte';
  import SharePopover from './SharePopover.svelte';
  import { statusLine } from '$lib/runs/status';
  import TryAgain from './TryAgain.svelte';
  import { sampleToFix, tryAgainModeOf } from './tryAgain';
  import WhatWentWrong from './WhatWentWrong.svelte';
  import AnalysisName from './AnalysisName.svelte';
  import AnalysisNav from './AnalysisNav.svelte';
  import ResumedNote from './ResumedNote.svelte';
  import { pipelineTitle, runName } from '$lib/runs/runName';
  import { runNames } from '$lib/runs/runNames.svelte';
  import { renameInNavList } from './analysisNav';
  import { site } from '$lib/state/site.svelte';
  import LiveProgress from './LiveProgress.svelte';
  import { stepProgress } from './liveProgress';
  import { authorLabels } from './stepLabel';
  import ExpertHint from '$lib/shell/ExpertHint.svelte';

  let { run }: { run: RunDetail } = $props();

  const ctx = getRunContext();
  const actions = ctx.actions;

  const live = $derived(isLiveRun(run));
  const ended = $derived(isTerminalRun(run));
  const line = $derived(statusLine(run));
  const order = $derived(sectionOrder(run.state));
  // A samplesheet to count against: progress in samples (and the samples grid shows each one).
  // Without one, progress is in steps, with each step's state in a strip.
  const sheet = $derived(!!run.samples?.declared && run.samples.total > 0);
  const steps = $derived(live && !sheet ? stepProgress(run) : null);
  const fraction = $derived(
    live ? (sheet ? progressFraction(run) : (steps?.fraction ?? null)) : null,
  );
  const cost = $derived(costText(run, !ended));
  const noCost = $derived(noCostLine({ live, pending: (run.cost?.pending_jobs ?? 0) > 0 }));
  const me = currentIdentity();

  // Keyed on primitives: the layout replaces `run` on every poll of a live run.
  const runId = $derived(run.run_id);
  const failed = $derived(run.state === 'failed');
  const versionId = $derived(run.workflow ?? null);
  // The exact version it ran, linked: name, short commit and the day it was registered.
  const versionHref = $derived(runVersionHref(run));
  const versionLabel = $derived(runVersionLabel(run));

  /**
   * Hand what `load` resolves to `ok` (or why it failed to `fail`) unless the effect that asked has
   * re-run or gone since; returns that effect's cleanup.
   */
  function whenCurrent<T>(
    load: Promise<T>,
    ok: (value: T) => void,
    fail: (e: unknown) => void = () => {},
  ): () => void {
    let stale = false;
    load
      .then((v) => {
        if (!stale) ok(v);
      })
      .catch((e) => {
        if (!stale) fail(e);
      });
    return () => {
      stale = true;
    };
  }

  // Why it failed, asked once per failed run.
  const why = new FailureSummaryLoader();
  $effect(() => why.watch(runId, failed));
  const summary = $derived(why.summary);
  const summaryError = $derived(why.error);

  // The run's version: its form reads the options the run changed, and Try again launches it.
  let version = $state<WorkflowDetail | null>(null);
  let versionError = $state<string | null>(null);
  $effect(() => {
    const id = versionId;
    version = null;
    versionError = null;
    if (!id) return;
    // Unloaded, Inputs used says so and shows less; Try again loads it again when pressed.
    return whenCurrent(
      getWorkflow(id),
      (v) => (version = v),
      (e) => (versionError = errorMessage(e)),
    );
  });

  // How long one run of its pipeline usually takes (from its past successful runs), asked once
  // while it runs: the time left is said only when there is one.
  let typical = $state<WorkflowEstimate['typical'] | null>(null);
  $effect(() => {
    const id = versionId;
    const ask = live;
    typical = null;
    if (!id || !ask) return;
    // Without a typical run, only the time so far is said.
    return whenCurrent(getWorkflowEstimate(id), (e) => (typical = e.typical ?? null));
  });
  const labels = $derived(authorLabels(version?.form_schema));

  // MultiQC's General Statistics, once the run has a MultiQC report and the page a QC section.
  const multiqcReport = $derived((ctx.reports ?? []).find((r) => r.kind === 'multiqc') ?? null);
  const hasMultiqc = $derived(multiqcReport != null);
  const showsQc = $derived(order.includes('qc'));
  let stats = $state<Stats | null>(null);
  $effect(() => {
    const id = runId;
    const ask = hasMultiqc && showsQc;
    stats = null;
    if (!ask) return;
    // Without General Statistics, the section is left out.
    return whenCurrent(getQcGeneralStats(id), (s) => {
      if (s.samples.length > 0) stats = s;
    });
  });

  // FastQC results and alignments to preview (QC at a glance), from the run's key results.
  const previews = $derived(qcChoices(ctx.reports ?? []));
  const hasPreviews = $derived(previews.fastqc.length > 0 || previews.alignments.length > 0);

  // Without FastQC results of its own, the run's MultiQC data may still say how each FastQC
  // module did (statuses only): asked once the run has a MultiQC report and no FastQC to open (a
  // boolean, so a reload of the reports that changes neither does not ask again). No
  // FastQC data (404) leaves the view out; any other failure (too large, the backend down) is said
  // in one line, so the view never vanishes without a word.
  let fastqcStatus = $state<FastqcStatusReport | null>(null);
  let fastqcStatusError = $state<string | null>(null);
  const askFastqcStatus = $derived(showsQc && hasMultiqc && previews.fastqc.length === 0);
  $effect(() => {
    const id = runId;
    const ask = askFastqcStatus;
    fastqcStatus = null;
    fastqcStatusError = null;
    if (!ask) return;
    return whenCurrent(
      getQcFastqcStatus(id),
      (s) => {
        if (s && s.samples.length > 0) fastqcStatus = s;
      },
      (e) => (fastqcStatusError = errorMessage(e)),
    );
  });

  // Try again: a failed run's mode from its failures (a fix wins); a cancelled run re-runs.
  const canTryAgain = $derived(ended && run.state !== 'succeeded' && !!run.workflow);
  const mode = $derived(tryAgainModeOf(summary?.items));
  const fixSample = $derived(sampleToFix(summary?.items));

  // The tab's title names the analysis, while the page shows it.
  const title = $derived(runName(run));
  $effect(() => {
    site.pageTitle = title;
    return () => {
      site.pageTitle = null;
    };
  });

  // A resumed analysis: the one it continues, to name it and tell whether they share a folder.
  const originId = $derived(run.resumed_from ?? null);
  let origin = $state<RunDetail | null>(null);
  $effect(() => {
    const id = originId;
    origin = null;
    if (!id) return;
    // Unloaded, it is named by its id, and the folder note is left out.
    return whenCurrent(getRun(id, { tasks: false }), (r) => (origin = r));
  });

  // Where "← Analyses" goes: the list the user came from, when the previous / next walk knows it.
  let backHref = $state('/runs');

  // A rename the server took shows at once (a finished run's page polls no more), outlives a poll
  // already in flight, and renames the run wherever this session has it by id (the Data page's
  // names, the remembered Analyses list).
  function renamed(summary: RunSummary) {
    ctx.applyRename(summary.run_id, summary.run_name?.trim() || null);
    runNames.put(summary);
    renameInNavList(summary.run_id, runName(summary));
  }

  function cancel() {
    const name = runName(run);
    void actions.cancel(run.run_id, `Cancel ${name}? The steps still running are stopped.`);
  }

  // A cancelled run says who stopped it, when the server knows.
  const cancelled = $derived(run.state === 'cancelled' ? cancelledBanner(run) : null);
  // The experiment it is in, named once its project's experiments have loaded.
  let experimentName = $state<string | null>(null);
  const bannerLabel = $derived(cancelled?.label ?? line.label);
  const bannerDetail = $derived.by(() => {
    if (cancelled) return cancelled.detail;
    if (run.state === 'succeeded' && cost)
      return [line.detail, cost.amount].filter(Boolean).join(' · ');
    // Running without a samplesheet: in steps against the known total ("2 of 5+ steps done"),
    // after the samples seen so far when its steps name any.
    if (steps) return (run.samples?.total ?? 0) > 0 ? `${line.detail} · ${steps.text}` : steps.text;
    return line.detail;
  });
</script>

<div class="topline">
  <a class="back" href={backHref}>← Analyses</a>
  <AnalysisNav runId={run.run_id} bind:backHref />
</div>
<header class="head">
  <div class="ident">
    <AnalysisName {run} onrenamed={renamed} />
    <p class="sub">
      {#if versionHref}<a
          href={versionHref}
          title={`Pipeline version ${run.workflow}`}
          data-testid="run-version"
          >{pipelineTitle(run) ?? 'Pipeline'}<span class="version">{' · '}{versionLabel}</span></a
        >{:else}Unknown pipeline{/if}
      · {startedLine(run, me)}
      {#if run.experiment_id}
        · In <a href={experimentHref(run.experiment_id)}>{experimentName ?? 'its experiment'}</a>
      {/if}
    </p>
  </div>
  <div class="actions">
    <SharePopover url={`${page.url.origin}${page.url.pathname}`} project={run.project} />
    <AddToExperiment
      runId={run.run_id}
      project={run.project}
      current={run.experiment_id}
      bind:currentName={experimentName}
    />
    {#if live}
      <button class="btn btn-danger" onclick={cancel} disabled={actions.cancelling}
        >{actions.cancelling ? 'Cancelling…' : 'Cancel'}</button
      >
    {/if}
    {#if canTryAgain && (!failed || summary || summaryError)}
      <TryAgain {run} {mode} {version} sample={fixSample} primary />
    {/if}
    {#if actions.error}<span class="error" role="alert">{actions.error}</span>{/if}
  </div>
</header>

<div class="banner {line.tone}" data-testid="analysis-status">
  <p>
    <strong>{bannerLabel}</strong>{#if bannerDetail}<span class="detail">{' · '}{bannerDetail}</span
      >{/if}
  </p>
  {#if live}
    <LiveProgress
      {run}
      {fraction}
      growing={steps?.growing ?? false}
      showSteps={!sheet}
      {typical}
      {labels}
      costSoFar={cost?.amount ?? null}
    />
  {/if}
</div>
<ResumedNote {run} {origin} />

{#each order as section (section)}
  {#if section === 'wrong'}
    <WhatWentWrong
      {run}
      {summary}
      error={summaryError}
      {mode}
      {version}
      {canTryAgain}
      sample={fixSample}
    />
  {:else if section === 'results'}
    <AnalysisResults {run} {version} />
  {:else if section === 'qc' && (stats || hasPreviews || fastqcStatus || fastqcStatusError)}
    <section id="qc" aria-labelledby="qc-h">
      <h2 id="qc-h">
        QC at a glance{#if stats}{' '}<span class="note">from the QC report</span>{/if}
      </h2>
      {#if stats}<QcGeneralStats runId={run.run_id} {stats} />{/if}
      {#if fastqcStatus}<FastqcFromMultiqc
          runId={run.run_id}
          report={fastqcStatus}
          multiqc={multiqcReport}
        />{/if}
      {#if fastqcStatusError}<p class="note fastqc-error" data-testid="fastqc-from-multiqc-error">
          FastQC per sample (from MultiQC) could not be shown: {fastqcStatusError}
        </p>{/if}
      {#if hasPreviews}<QcPreviews runId={run.run_id} reports={ctx.reports ?? []} />{/if}
    </section>
  {:else if section === 'samples'}
    <SampleGrid runId={run.run_id} {live} id="samples" />
  {:else if section === 'inputs'}
    <InputsUsed {run} {version} {versionError} {origin} />
  {:else if section === 'cost'}
    <section id="cost" aria-labelledby="cost-h">
      <h2 id="cost-h">Cost</h2>
      {#if cost}
        <p class="amount">{cost.amount}</p>
        <p class="note">{cost.note}</p>
      {:else}
        <p class="note">{noCost.title}.</p>
      {/if}
    </section>
  {/if}
{/each}
<ExpertHint />

<style>
  .topline {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem 1rem;
  }
  .back {
    font-size: 0.8rem;
    color: var(--text-muted);
    text-decoration: none;
  }
  .head {
    position: sticky;
    top: 0;
    z-index: 10;
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.6rem 1rem;
    margin: 0.3rem 0 0.8rem;
    padding: 0.4rem 0;
    background: var(--bg-content);
    border-bottom: 1px solid var(--border);
  }
  .ident {
    min-width: 0;
  }
  .sub {
    margin: 0.2rem 0 0;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .sub a {
    color: var(--accent);
    text-decoration: underline;
  }
  .version {
    font-size: 0.78rem;
  }
  .error {
    font-size: 0.78rem;
  }
  .banner {
    margin: 0 0 1.25rem;
    padding: 0.7rem 1rem;
    border: 1px solid var(--card-border);
    border-left-width: 4px;
    border-radius: 0.55rem;
  }
  .banner p {
    margin: 0;
  }
  .banner.running {
    border-left-color: var(--accent);
  }
  .banner.done {
    border-left-color: var(--fg-ok);
  }
  .banner.attention {
    border-left-color: var(--fg-fail);
  }
  .banner.running strong {
    color: var(--accent);
  }
  .banner.done strong {
    color: var(--fg-ok);
  }
  .banner.attention strong {
    color: var(--fg-fail);
  }
  .detail {
    color: var(--text);
  }
  section {
    margin-bottom: 1.5rem;
  }
  section h2 {
    margin: 0 0 0.6rem;
  }
  .note {
    margin: 0;
    font-size: 0.8rem;
    font-weight: normal;
    color: var(--text-muted);
  }
  .fastqc-error {
    margin-top: 0.75rem;
  }
  .amount {
    margin: 0 0 0.2rem;
    font-size: 1.5rem;
    font-weight: 600;
  }
</style>
