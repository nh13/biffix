<script lang="ts">
  import { listTasks, type RunDetail, type TaskPage } from '$lib/api';
  import { fmtWallTime } from '$lib/util/format';
  import { terminationLabel } from '$lib/runs/attempts';
  import { startPoll } from '$lib/util/poll';
  import { isLiveRun } from '$lib/runs/runState';
  import { jobsHref } from '$lib/runs/jobsUrl';
  import { uniqueBy } from '$lib/runs/taskWindow';

  type Task = TaskPage['tasks'][number];

  // What to look at first: failed jobs, then those still running or queued.
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  const SHOWN = 5;
  let failed = $state<Task[]>([]);
  let failedTotal = $state(0);
  let active = $state<Task[]>([]);
  let activeTotal = $state(0);
  let loaded = $state(false);
  let error = $state<string | null>(null);

  // Primitives read off the run: the layout replaces `run` on every poll, and an effect that read
  // the object would restart this poll each time.
  const runId = $derived(run.run_id);
  const live = $derived(isLiveRun(run));

  $effect(() => {
    const id = runId;
    return startPoll({
      fetch: () =>
        Promise.all([
          listTasks(id, { state: 'failed', limit: SHOWN }),
          listTasks(id, { state: 'active', sort: 'started', limit: SHOWN }),
        ]),
      onData: ([f, a]) => {
        failed = f.tasks;
        failedTotal = f.total;
        active = a.tasks;
        activeTotal = a.total;
        loaded = true;
        error = null;
      },
      onError: (m) => (error = m),
      // Read as each load lands (not a dependency): the run ending stops the poll.
      until: () => !live,
      intervalMs: 5000,
    });
  });

  // The two lists are separate reads, so a task that changed state between them (a retried
  // failure, now queued) can be in both: shown once, as the failed read has it.
  const shown = $derived(uniqueBy([...failed, ...active], (t) => t.task_key));

  const allJobs = $derived(jobsHref(runId));
</script>

<section class="attention">
  <h2>Needs attention</h2>
  {#if error && !loaded}
    <p class="error">Couldn't load jobs: {error}</p>
  {:else if !loaded}
    <p class="muted">Loading…</p>
  {:else if shown.length === 0}
    <p class="muted empty">
      {(run.tasks_observed ?? 0) === 0
        ? 'No jobs yet.'
        : 'Nothing: no job failed and none is still running.'}
      <a href={allJobs}>All jobs →</a>
    </p>
  {:else}
    <ul>
      {#each shown as t (t.task_key)}
        {@const termination = terminationLabel(t)}
        <li>
          <span class={`badge ${t.state}`}>{t.state}</span>
          <a class="key" href={jobsHref(runId, { job: t.task_key })}>{t.task_key}</a>
          <span class="muted detail">
            {#if termination}{termination} ·{/if}
            attempt {t.attempts} · {fmtWallTime(t.started_at, t.finished_at)}
          </span>
        </li>
      {/each}
    </ul>
    <p class="more muted">
      {#if failedTotal > failed.length}<a href={jobsHref(runId, { state: 'failed' })}
          >All {failedTotal} failed →</a
        >{/if}
      {#if activeTotal > active.length}<a href={jobsHref(runId, { state: 'active' })}
          >All {activeTotal} active →</a
        >{/if}
      <a href={allJobs}>All jobs →</a>
    </p>
  {/if}
</section>

<style>
  .attention {
    margin-bottom: 1.5rem;
  }
  ul {
    margin: 0;
    padding: 0.4rem 0.9rem;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.25rem 0.6rem;
    padding: 0.3rem 0;
    font-size: 0.82rem;
  }
  li + li {
    border-top: 1px solid var(--border);
  }
  .key {
    font-family: var(--mono);
    font-size: 0.8rem;
    word-break: break-all;
  }
  .detail {
    font-size: 0.75rem;
  }
  /* A link inside a sentence must not rely on color alone to stand out (WCAG 1.4.1). */
  .empty a {
    text-decoration: underline;
  }
  .more {
    display: flex;
    gap: 1rem;
    font-size: 0.8rem;
    margin: 0.4rem 0 0;
  }
</style>
