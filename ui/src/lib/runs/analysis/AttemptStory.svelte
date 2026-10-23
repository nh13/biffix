<script lang="ts">
  // A failed step's tries in *Why?*, one plain line each (`attemptStory`): when, on what kind of
  // machine, why it ended, and whether it was tried again. Read from the step's attempt records,
  // asked once per step; until they come (or if they cannot), the count of tries the summary gave.
  import { getAttempts, type TaskAttempt } from '$lib/api';
  import { attemptStory, tryText } from './attemptStory';

  interface Props {
    runId: string;
    /** The failed task; without one (the engine's own failure) only the count is said. */
    taskKey?: string | null;
    /** How many tries the failure summary counted. */
    count: number;
    /** The analysis has ended: a try still open in the records is told as left without a result. */
    runEnded?: boolean;
  }
  let { runId, taskKey = null, count, runEnded = false }: Props = $props();

  let attempts = $state<TaskAttempt[] | null>(null);
  $effect(() => {
    const run = runId;
    const task = taskKey;
    attempts = null;
    if (!task) return;
    let cancelled = false;
    getAttempts(run, task)
      .then((a) => {
        if (!cancelled) attempts = a.attempts;
      })
      .catch(() => {
        /* the count stands */
      });
    return () => {
      cancelled = true;
    };
  });

  const lines = $derived(attempts && attempts.length ? attemptStory(attempts, { runEnded }) : []);
  const countText = $derived(count === 1 ? 'Once' : count === 0 ? 'None' : `${count} times`);
</script>

{#if lines.length}
  <ol class="tries" data-testid="attempt-story">
    {#each lines as line, i (i)}
      <li class={line.tone}>{tryText(line)}</li>
    {/each}
  </ol>
{:else}
  {countText}
{/if}

<style>
  .tries {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 0.2rem;
  }
  .tries li {
    padding-left: 0.6rem;
    border-left: 3px solid var(--card-border);
  }
  .tries li.bad {
    border-left-color: var(--fg-fail);
  }
  .tries li.ok {
    border-left-color: var(--fg-ok);
  }
  .tries li.live {
    border-left-color: var(--accent);
  }
</style>
