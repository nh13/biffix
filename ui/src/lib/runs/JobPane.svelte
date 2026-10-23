<script lang="ts">
  import type { RunDetail, TaskPage } from '$lib/api';
  import { fmtWallTime, plural } from '$lib/util/format';
  import { jobCostText } from '$lib/runs/cost';
  import { onMount } from 'svelte';
  import { batchJobUrl, cloudwatchLogUrl } from '$lib/util/links';
  import { deploy, loadDeploy } from '$lib/state/deploy.svelte';
  import {
    capacityLabel,
    hostCapacityLabel,
    hostLabel,
    requestedLabel,
    terminationLabel,
    verdictLabel,
  } from '$lib/runs/attempts';
  import { isLiveRun } from '$lib/runs/runState';
  import TaskPanel from '$lib/runs/TaskPanel.svelte';
  import RunOutputs from '$lib/runs/RunOutputs.svelte';
  import { runMayHaveOutputs } from '$lib/runs/runNav';

  type Task = TaskPage['tasks'][number];

  // One job's detail beside (or, when narrow, below) the jobs table: its facts and links, what it
  // needs and feeds, and its logs (every attempt) or outputs.
  interface Props {
    run: RunDetail;
    task: Task;
    /** The CloudWatch log group of the run's task streams (`TaskPage.log_group`; none ⇒ `/aws/batch/job`). */
    logGroup?: string | null;
    /** Select another job (a dependency link). */
    onselect: (taskKey: string) => void;
    onclose: () => void;
  }
  let { run, task, logGroup = null, onselect, onclose }: Props = $props();

  let panel = $state<'logs' | 'outputs'>('logs');
  const hasOutputs = $derived(runMayHaveOutputs(run));

  // Console links need the deploy's region (from /auth/config); without it they are left out.
  onMount(loadDeploy);
  const batchUrl = $derived(batchJobUrl(task.batch_job_id, deploy.region));
  const logUrl = $derived(cloudwatchLogUrl(task.log_stream, deploy.region, logGroup));
  const requested = $derived(requestedLabel(task));
  // The host's own capacity, shown beside the request so an undersized request is plain to see.
  const hostCapacity = $derived(hostCapacityLabel(task.host));
  const termination = $derived(terminationLabel(task));
  const host = $derived(hostLabel(task.host));
  const capacity = $derived(capacityLabel(task.host));
  const verdict = $derived(verdictLabel(task));
</script>

<aside class="pane" aria-label={`Job ${task.task_key}`}>
  <header>
    <div class="title">
      <h3 class="mono">{task.task_key}</h3>
      <div class="facts">
        <span class={`badge ${task.state}`}>{task.state}</span>
        {#if task.cached}<span class="muted">cached</span>{/if}
        {#if termination}<span class="termination">{termination}</span>{/if}
        <span class="muted"
          >{plural(task.attempts, 'attempt')} · {fmtWallTime(
            task.started_at,
            task.finished_at,
          )}</span
        >
        {#if task.exit_code != null}<span class:nonzero={task.exit_code !== 0} class="muted"
            >exit {task.exit_code}</span
          >{/if}
      </div>
      <div class="facts">
        <!-- Only a Batch job has a request: a cache hit or local rule shows none, not "unknown". -->
        {#if task.batch_job_id}<span class="mono muted"
            >requested {#if requested}{requested}{:else}<span aria-hidden="true">--</span><span
                class="sr-only">unknown</span
              >{/if}</span
          >{/if}
        {#if hostCapacity}<span class="mono muted">host has {hostCapacity}</span>{/if}
        {#if host}<span class="mono muted">{host}</span>{/if}
        {#if capacity}<span class="mono muted">{capacity}</span>{/if}
        {#if task.cost_usd != null}<span class="muted" title="Estimated, all attempts"
            >{jobCostText(task.cost_usd)}</span
          >{/if}
        {#if batchUrl}<a class="link" href={batchUrl} target="_blank" rel="noreferrer"
            >Batch job ↗</a
          >{/if}
        {#if logUrl}<a
            class="link"
            href={logUrl}
            target="_blank"
            rel="noreferrer"
            title="The raw stream in the AWS console (requires AWS access)">CloudWatch ↗</a
          >{/if}
      </div>
      {#if task.image}<div class="facts">
          <span class="mono muted image" title="The container image it ran in">{task.image}</span>
        </div>{/if}
      {#if verdict}<p class="verdict">{verdict}</p>{/if}
    </div>
    <button class="close" onclick={onclose} aria-label="Close job details" title="Close (Esc)"
      >✕</button
    >
  </header>

  {#each [['needs', task.deps ?? []], ['feeds', task.feeds ?? []]] as [label, keys]}
    {#if keys.length > 0}
      <div class="deps">
        <span class="deps-label">{label}</span>
        {#each keys as k (k)}
          <button class="linkish dep" onclick={() => onselect(k)}>{k}</button>
        {/each}
      </div>
    {/if}
  {/each}

  <!-- Toggle buttons (aria-pressed), as TaskPanel's attempt picker: each is its own tab stop. -->
  <div class="panel-tabs" role="group" aria-label="Job details">
    <button aria-pressed={panel === 'logs'} onclick={() => (panel = 'logs')}>Logs</button>
    {#if hasOutputs}
      <button aria-pressed={panel === 'outputs'} onclick={() => (panel = 'outputs')}>Outputs</button
      >
    {/if}
  </div>

  <div class="panel">
    {#key task.task_key}
      {#if panel === 'outputs' && hasOutputs}
        <RunOutputs
          runId={run.run_id}
          project={run.project}
          engine={run.engine}
          outputsTotal={run.outputs_total ?? 0}
          live={isLiveRun(run)}
          task={task.task_key}
          onclearTask={() => (panel = 'logs')}
          onjumpTask={onselect}
        />
      {:else if task.batch_job_id}
        <TaskPanel
          runId={run.run_id}
          taskKey={task.task_key}
          attempts={task.attempts}
          active={task.state === 'running'}
        />
      {:else}
        <p class="muted">
          {task.cached
            ? 'Satisfied from the engine cache: it did not run, so it has no logs.'
            : 'No Batch job yet (or a local rule), so there are no logs.'}
        </p>
      {/if}
    {/key}
  </div>
</aside>

<style>
  .pane {
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
    padding: 0.75rem 0.9rem;
    min-width: 0;
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: 0.75rem;
  }
  .title {
    min-width: 0;
  }
  h3 {
    margin: 0 0 0.3rem;
    font-size: 0.9rem;
    word-break: break-all;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.35rem 0.6rem;
    font-size: 0.75rem;
    margin-top: 0.2rem;
  }
  .nonzero {
    color: var(--fg-fail);
    font-weight: 600;
  }
  .termination {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--fg-fail);
  }
  .image {
    word-break: break-all;
  }
  .verdict {
    margin: 0.35rem 0 0;
    font-size: 0.75rem;
    color: var(--fg-fail);
  }
  .close {
    align-self: flex-start;
    background: none;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: var(--text-muted);
  }
  .close:hover {
    color: var(--text);
  }
  .deps {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.15rem 0.6rem;
    font-size: 0.72rem;
    color: var(--text-muted);
    margin-top: 0.5rem;
  }
  .deps-label {
    min-width: 2.6rem;
  }
  .dep {
    font-family: var(--mono);
    font-size: 0.72rem;
  }
  .panel-tabs {
    display: flex;
    gap: 0.25rem;
    margin: 0.75rem 0 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  .panel-tabs button {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    padding: 0.3rem 0.6rem;
    font: inherit;
    font-size: 0.8rem;
    color: var(--text-muted);
    cursor: pointer;
  }
  .panel-tabs button[aria-pressed='true'] {
    color: var(--text);
    border-bottom-color: var(--accent, #4a7cf5);
  }
  .panel :global(.outputs) {
    margin: 0;
  }
  .linkish {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
    font: inherit;
  }
  .link {
    font-size: 0.72rem;
    text-decoration: none;
    border-bottom: 1px dotted currentColor;
  }
</style>
