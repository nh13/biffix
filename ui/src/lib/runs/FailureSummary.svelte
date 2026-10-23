<script lang="ts">
  // Why a failed run failed: its first failed steps, each in plain words — what
  // happened, what to try — with the end of its log and a link to the job; or the workflow engine's
  // own failure when no step failed.
  import type { FailureSummary, RunDetail } from '#lib/api/index.js';
  import { FailureSummaryLoader } from '#lib/runs/failureLoader.svelte.js';
  import { jobsHref } from '#lib/runs/jobsUrl.js';
  import { runTabHref } from '#lib/runs/runNav.js';
  import { plural } from '#lib/util/format.js';

  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  // Primitives read off the run (the layout replaces `run` on every poll), so the summary is asked
  // for once per run that failed, not on every poll.
  const runId = $derived(run.run_id);
  const failed = $derived(run.state === 'failed');
  const why = new FailureSummaryLoader();
  $effect(() => why.watch(runId, failed));
  const summary = $derived(why.summary);
  const error = $derived(why.error);

  const more = $derived(summary ? summary.failed_steps - summary.items.length : 0);
  // A run that failed before it launched (preparing its images, or launching), as the server marks
  // it with the stage it stopped at: the control plane's own reason is the message, and there is no
  // job or engine log to open.
  const beforeLaunch = (item: FailureSummary['items'][number]) => item.stage != null;
</script>

{#if failed && (summary?.items.length || error)}
  <section class="failure" aria-labelledby="failure-title">
    <h2 id="failure-title">Why this run failed</h2>
    {#if error}
      <p class="muted">Couldn't work out why: {error}</p>
    {/if}
    <!-- Keyed by position: two items without a task key can share a step name. -->
    {#each summary?.items ?? [] as item, i (i)}
      <article class="item">
        <p class="what">
          <strong>{item.step}</strong>{#if item.task_key && item.task_key !== item.step}{' '}<span
              class="key">({item.task_key})</span
            >{/if}{' '}failed: {item.message}{#if item.attempts > 1}{' '}<span class="muted"
              >· after {item.attempts} tries</span
            >{/if}
        </p>
        <p class="advice">{item.advice}</p>
        {#if item.reason && item.log_tail.length === 0 && !beforeLaunch(item)}
          <p class="reason"><span class="muted">AWS says:</span> <code>{item.reason}</code></p>
        {/if}
        {#if item.log_tail.length > 0}
          <details>
            <summary>Last {item.log_tail.length} log lines</summary>
            <pre>{item.log_tail.join('\n')}</pre>
          </details>
        {/if}
        {#if !beforeLaunch(item)}
          <a
            class="open"
            href={item.task_key
              ? jobsHref(runId, { job: item.task_key })
              : runTabHref(runId, 'coordinator')}
            >{item.task_key ? 'Open the job →' : 'Open the engine log →'}</a
          >
        {/if}
      </article>
    {/each}
    {#if more > 0}
      <p class="muted">
        {plural(more, 'more failed step')}.
        <a href={jobsHref(runId, { state: 'failed' })}>All failed jobs →</a>
      </p>
    {/if}
  </section>
{/if}

<style>
  .failure {
    border: 1px solid var(--fg-fail);
    border-left-width: 4px;
    border-radius: 0.5rem;
    padding: 0.75rem 1rem;
    margin: 0 0 1rem;
  }
  h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  .item + .item {
    margin-top: 0.8rem;
    padding-top: 0.8rem;
    border-top: 1px solid var(--border);
  }
  .what {
    margin: 0;
  }
  .key {
    font-family: var(--mono);
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .advice {
    margin: 0.25rem 0 0;
    font-size: 0.88rem;
  }
  details {
    margin-top: 0.4rem;
    font-size: 0.82rem;
  }
  summary {
    cursor: pointer;
    color: var(--text-muted);
  }
  pre {
    margin: 0.35rem 0 0;
    padding: 0.5rem 0.65rem;
    max-height: 16rem;
    overflow: auto;
    background: var(--hover);
    border-radius: 0.35rem;
    font-family: var(--mono);
    font-size: 0.75rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .open,
  a {
    color: var(--accent);
    font-size: 0.85rem;
  }
  .open {
    display: inline-block;
    margin-top: 0.35rem;
  }
  .muted {
    font-size: 0.85rem;
  }
</style>
