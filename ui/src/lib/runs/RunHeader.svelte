<script lang="ts">
  import { onMount } from 'svelte';
  import { experimentHref } from '#lib/experiments/experiments.js';
  import type { RunDetail } from '#lib/api/index.js';
  import { fmtDate, fmtRelativeMicros, fmtUsd, fmtWallTime, plural } from '#lib/util/format.js';
  import { batchJobUrl, cognitoUsersUrl } from '#lib/util/links.js';
  import { deploy, loadDeploy } from '#lib/state/deploy.svelte.js';
  import { isLiveRun } from '#lib/runs/runState.js';
  import StatusPill from '#lib/runs/StatusPill.svelte';
  import { capacityLabel, hostLabel } from '#lib/runs/attempts.js';
  import { costBasis } from '#lib/runs/runFacts.js';
  import { runVersionHref, runVersionLabel } from '#lib/runs/runVersion.js';
  import { imageLabel, isBuilt, shortDigest } from '#lib/workflows/readiness.js';
  import Progress from '#lib/components/Progress.svelte';
  import RunActions from '#lib/runs/RunActions.svelte';
  import AddToExperiment from '#lib/experiments/AddToExperiment.svelte';

  // The run's identity, state and headline facts, with the Cancel action while it is live, and the
  // experiment it is in (put it in one, move it, take it out).
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  // The Cognito user pool id and the deploy's region (public, from /auth/config) link a user
  // submitter and the coordinator job to the AWS console. Fetched once; a miss just means no link.
  onMount(loadDeploy);

  // Only a human submitter (`user:<sub>`) links to Cognito; a `watcher:<id>` does not.
  const submitterUrl = $derived(
    run.submitter?.startsWith('user:') ? cognitoUsersUrl(deploy.userPoolId, deploy.region) : null,
  );
  const coordinatorUrl = $derived(batchJobUrl(run.coordinator_job_id, deploy.region));
  const coordinatorHost = $derived(hostLabel(run.coordinator_host));
  const coordinatorCapacity = $derived(capacityLabel(run.coordinator_host));
  const basis = $derived(costBasis(run.cost));
  // A run waiting for its submit-time images to be imported (and given the engine runtime): why it
  // has not launched yet, and each image's progress.
  const preparing = $derived(run.preparing ?? null);
  // The experiment it is in, named once its project's experiments have loaded.
  let experimentName = $state<string | null>(null);
</script>

<a class="back" href="/runs">← Runs</a>

<header class="run-header">
  <div>
    {#if run.run_name}
      <h1>{run.run_name}</h1>
      <div class="run-id">{run.run_id}</div>
    {:else}
      <h1>{run.run_id}</h1>
    {/if}
    <div class="tags">
      <!-- The version once: one link (its name and label), then its full id as text to copy. -->
      {#if run.workflow}<a class="workflow" href={runVersionHref(run)} data-testid="run-version"
          >{run.workflow_name ?? 'Pipeline'} · {runVersionLabel(run)}</a
        ><span class="tag version" title="Frozen version id" data-testid="run-version-id"
          >{run.workflow}</span
        >{/if}
      {#if run.engine}<span class="tag engine">{run.engine}</span>{/if}
      {#if run.project}<span class="tag project">{run.project}</span>{/if}
      {#if run.experiment_id}<span class="in-exp"
          >In <a href={experimentHref(run.experiment_id)}>{experimentName ?? run.experiment_id}</a
          ></span
        >{/if}
    </div>
  </div>
  <div class="state">
    <StatusPill state={run.state} polite />
    {#if isLiveRun(run)}<span class="live" title="Auto-refreshing"
        ><span class="dot" aria-hidden="true">●</span> live</span
      >{/if}
    <AddToExperiment
      runId={run.run_id}
      project={run.project}
      current={run.experiment_id}
      bind:currentName={experimentName}
    />
    <RunActions {run} />
  </div>
</header>

{#if preparing}
  <section class="preparing" aria-labelledby="preparing-heading" data-testid="preparing">
    <h2 id="preparing-heading">Preparing images</h2>
    <p class="hint-line">
      Waiting since {fmtRelativeMicros(preparing.since_micros)} for the images this run was submitted
      with. It launches once every one is ready.
    </p>
    {#if preparing.not_ready.length > 0}
      <ul class="not-ready">
        {#each preparing.not_ready as reason}<li>{reason}</li>{/each}
      </ul>
    {/if}
    {#if preparing.images.length > 0}
      <ul class="images">
        {#each preparing.images as image (image.declared_ref)}
          <li>
            <span class="mono ref">{image.declared_ref}</span>
            <span class="img-state {image.state}">{imageLabel(image)}</span>
            <span class="mono digest"
              >{shortDigest(image.image ?? (isBuilt(image) ? null : image.source_ref))}</span
            >
            {#if image.logs_url}<a href={image.logs_url} target="_blank" rel="noopener"
                >Build log ↗</a
              >{/if}
            {#if image.reason}<div class="img-reason">{image.reason}</div>{/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}

<div class="meta">
  <div class="meta-item">
    <span class="meta-label">Submitted</span>
    <span class="meta-value">{fmtDate(run.submitted_at)}</span>
  </div>
  <div class="meta-item">
    <span class="meta-label">Wall time</span>
    <span class="meta-value">{fmtWallTime(run.submitted_at, run.completed_at)}</span>
  </div>
  <div class="meta-item">
    <span class="meta-label">Submitter</span>
    <span class="meta-value" title={run.submitter ?? undefined}>
      {#if submitterUrl}
        <a href={submitterUrl} target="_blank" rel="noreferrer"
          >{run.submitter_label ?? run.submitter}</a
        >
      {:else}{run.submitter_label ?? run.submitter ?? '--'}{/if}
    </span>
  </div>
  <div class="meta-item">
    <span class="meta-label">Progress</span>
    <Progress {run} />
  </div>
  {#if run.coordinator_job_id}
    <div class="meta-item">
      <span class="meta-label">Coordinator</span>
      <span class="meta-value coordinator">
        {#if coordinatorUrl}<a href={coordinatorUrl} target="_blank" rel="noreferrer">Batch job →</a
          >{/if}
        {#if coordinatorHost}<span class="instance">{coordinatorHost}</span>{/if}
        {#if coordinatorCapacity}<span class="capacity">{coordinatorCapacity}</span>{/if}
      </span>
    </div>
  {/if}
  {#if run.cost}
    <div class="meta-item">
      <span class="meta-label">Est. cost</span>
      <span class="meta-value" title={basis}>
        <!-- An amount only once a job is priced (the lists' rule); before, only what is pending. -->
        {#if run.cost.priced_jobs > 0}
          {fmtUsd(run.cost.estimated_usd)}
          <span class="tag estimate">estimated</span>
        {:else}
          <span class="no-cost">No cost yet</span>
        {/if}
        {#if run.cost.pending_jobs > 0}<span class="hint pending-jobs"
            >{run.cost.priced_jobs > 0 ? '+' : ''}{plural(run.cost.pending_jobs, 'job')} pending</span
          >{/if}
      </span>
    </div>
  {/if}
</div>

<style>
  .run-id {
    font-family: var(--mono);
    font-size: 0.76rem;
    color: var(--text-muted);
    margin-top: 0.15rem;
  }
  .back {
    font-size: 0.8rem;
    color: var(--text-muted);
    text-decoration: none;
  }
  .back:hover {
    color: var(--text-nav);
  }
  .run-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    margin: 0.5rem 0 1rem;
  }
  .run-header h1 {
    margin: 0;
    font-size: 1.25rem;
  }
  .workflow {
    font-size: 0.78rem;
    color: var(--accent);
  }
  .tags {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
    margin-top: 0.2rem;
  }
  .state {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    align-items: center;
    gap: 0.5rem;
  }
  .live {
    white-space: nowrap;
    font-size: 0.7rem;
    color: var(--text-muted);
  }
  /* Only the dot pulses: fading the text would drop it below readable contrast. */
  .dot {
    display: inline-block;
    animation: pulse 1.6s ease-in-out infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .pending-jobs {
    white-space: nowrap;
  }
  .no-cost {
    color: var(--text-muted);
  }
  .meta {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 0.6rem;
    margin-bottom: 1.5rem;
    padding: 0.75rem 0.9rem;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .meta-item {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .meta-label {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
  .meta-value {
    font-size: 0.88rem;
    color: var(--text);
  }
  /* The Batch job link sits in a line with the host's text: it must not rely on color alone to
     stand out (WCAG 1.4.1). */
  .coordinator a {
    text-decoration: underline;
  }
  .tag {
    font-size: 0.68rem;
    padding: 0.05rem 0.4rem;
    border-radius: 0.3rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
    text-transform: lowercase;
  }
  .in-exp {
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .tag.engine {
    border-color: #7c6cf5;
    color: var(--fg-engine);
  }
  .tag.version {
    font-family: var(--mono);
    text-transform: none;
    user-select: all;
  }
  .tag.estimate {
    text-transform: none;
    margin-left: 0.3rem;
  }
  /* Inline after a label or a heading, so a step smaller than the shared hint. */
  .hint {
    font-size: 0.72rem;
    font-weight: normal;
    margin-left: 0.3rem;
  }
  .preparing {
    margin-bottom: 1rem;
    padding: 0.75rem 0.9rem;
    border: 1px solid var(--border);
    border-left: 3px solid var(--fg-warn);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .preparing h2 {
    margin: 0 0 0.3rem;
    font-size: 0.95rem;
  }
  .hint-line {
    margin: 0 0 0.4rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .not-ready,
  .images {
    margin: 0 0 0.4rem;
    padding-left: 1.1rem;
    font-size: 0.82rem;
  }
  .images {
    list-style: none;
    padding-left: 0;
  }
  .images li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0.2rem 0;
    border-top: 1px solid var(--border);
  }
  .mono {
    font-size: 0.76rem;
    word-break: break-all;
  }
  .digest {
    color: var(--text-muted);
  }
  .img-state {
    font-size: 0.72rem;
    padding: 0 0.35rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border);
    color: var(--fg-warn);
  }
  .img-state.ready {
    color: var(--fg-ok);
  }
  .img-state.failed {
    color: var(--fg-fail);
  }
  .img-reason {
    flex-basis: 100%;
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .instance,
  .capacity {
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--text-muted);
  }
</style>
