<script lang="ts">
  import { getAttempts, getTaskLogs, type TaskAttempt } from '$lib/api';
  import { attemptLabel } from '$lib/runs/attempts';
  import LogStream from '$lib/components/LogStream.svelte';
  import { errorMessage } from '$lib/util/format';

  // The expanded panel under a task row: its logs, plus — when the task ran more than once (e.g. a
  // spot reclaim then a retry) — a picker to read any attempt's logs, not just the one that won.
  interface Props {
    runId: string;
    taskKey: string;
    /** The task's attempt count; the picker is fetched only when there is more than one. */
    attempts: number;
    /** Whether the task is still in flight — drives live-tail polling of the display attempt. */
    active?: boolean;
  }
  let { runId, taskKey, attempts, active = false }: Props = $props();

  let list = $state<TaskAttempt[]>([]);
  let error = $state<string | null>(null);
  // Whether an attempt list has come back (or failed) — until then the display attempt is unknown.
  let listed = $state(false);
  // The picked attempt's job id; undefined ⇒ the display attempt (what the task row summarizes).
  let selected = $state<string | undefined>(undefined);

  $effect(() => {
    if (attempts <= 1) return;
    // A newer attempt count re-fetches; an older, slower response must not land over it.
    let stale = false;
    getAttempts(runId, taskKey)
      .then((res) => {
        if (stale) return;
        list = res.attempts;
        error = null;
      })
      .catch((e) => {
        if (stale) return;
        error = errorMessage(e);
      })
      .finally(() => {
        if (!stale) listed = true;
      });
    return () => {
      stale = true;
    };
  });

  const displayJob = $derived(list.find((a) => a.display)?.batch_job_id);
  // The attempt whose logs show, named explicitly once known: when a retry moves the display
  // attempt, this changes and the viewer starts over on the new attempt's stream rather than
  // tailing the old one's cursor.
  const job = $derived(selected ?? displayJob);
  // Only the display attempt can still be running; an older attempt's stream is final.
  const jobIsDisplay = $derived(selected === undefined || selected === displayJob);
  // With several attempts, wait for the list so the viewer opens once, on the right attempt.
  const ready = $derived(attempts <= 1 || listed);
</script>

{#if list.length > 1}
  <!-- Toggle buttons (aria-pressed): each is its own tab stop, Enter/Space picks it. -->
  <div class="picker" role="group" aria-label="Attempts">
    {#each list as a (a.batch_job_id)}
      <button
        class="attempt {a.state}"
        aria-pressed={a.batch_job_id === job}
        title={a.status_reason ?? a.batch_job_id}
        onclick={() => (selected = a.display ? undefined : a.batch_job_id)}
      >
        {attemptLabel(a)}{a.display ? ' ✓' : ''}
      </button>
    {/each}
  </div>
{:else if error}
  <p class="hint">Couldn't load attempts: {error}</p>
{/if}

{#if ready}
  {#key job}
    <LogStream
      fetchPage={(opts) => getTaskLogs(runId, taskKey, { ...opts, job })}
      active={active && jobIsDisplay}
    />
  {/key}
{/if}

<style>
  .picker {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
    margin: 0.35rem 0 0.25rem;
  }
  .attempt {
    font-size: 0.72rem;
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
  }
  .attempt[aria-pressed='true'] {
    color: inherit;
    border-color: #4a7cf5;
  }
  .attempt.failed {
    border-style: dashed;
  }
  .hint {
    margin: 0.25rem 0;
  }
</style>
