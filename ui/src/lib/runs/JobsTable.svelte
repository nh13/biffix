<script lang="ts">
  // The Jobs view's table: one row per job of the loaded window (clicking one selects it, opening
  // its pane), and a footer counting them with Show more.
  import type { TaskPage } from '$lib/api';
  import { fmtWallTime } from '$lib/util/format';
  import { hostLabel, requestedLabel, terminationLabel } from './attempts';
  import { NO_BATCH_JOB } from './jobsTable';
  import { jobCostText } from './cost';

  type Task = TaskPage['tasks'][number];

  interface Props {
    rows: Task[];
    /** How many jobs match the filters (the table holds the first `rows.length`). */
    total: number;
    /** The selected job's key. */
    selected: string | null;
    onselect: (taskKey: string | null) => void;
    /** How many more rows Show more loads. */
    page: number;
    loadingMore: boolean;
    onshowmore: () => void;
  }
  let { rows, total, selected, onselect, page, loadingMore, onshowmore }: Props = $props();
</script>

<!-- The table's `--` placeholder, read out as `label` rather than as punctuation. -->
{#snippet dash(label: string)}<span aria-hidden="true">--</span><span class="sr-only">{label}</span
  >{/snippet}

<table>
  <thead>
    <tr>
      <th>Task</th>
      <th>State</th>
      <th class="num narrow-hide">Attempts</th>
      <th class="num">Duration</th>
      <th class="num narrow-hide">Exit</th>
      <th class="wide" title="What each job asked AWS Batch for (not what its host has)"
        >Requested</th
      >
      <th class="wide">Host</th>
      <th class="num wide">Cost</th>
    </tr>
  </thead>
  <tbody>
    {#each rows as t, i (t.task_key)}
      {@const termination = terminationLabel(t)}
      {@const requested = requestedLabel(t)}
      {@const host = hostLabel(t.host)}
      <tr
        id={`job-row-${i}`}
        class:selected={selected === t.task_key}
        onclick={() => onselect(t.task_key)}
      >
        <td class="task-key">
          <button
            class="linkish key"
            aria-pressed={selected === t.task_key}
            onclick={(e) => {
              e.stopPropagation();
              onselect(selected === t.task_key ? null : t.task_key);
            }}>{t.task_key}</button
          >
        </td>
        <td>
          <span class={`badge ${t.state}`}>{t.state}</span>
          {#if t.cached}<span class="cached" title="Satisfied from the engine cache">cached</span
            >{/if}
          {#if termination}<span class="termination">{termination}</span>{/if}
        </td>
        <td class="num narrow-hide">{t.attempts}</td>
        <td class="num">{fmtWallTime(t.started_at, t.finished_at)}</td>
        <td class="num exit narrow-hide" class:nonzero={t.exit_code != null && t.exit_code !== 0}
          >{#if t.exit_code != null}{t.exit_code}{:else if !t.batch_job_id}{@render dash(
              NO_BATCH_JOB,
            )}{:else}--{/if}</td
        >
        <!-- `requested`: the e2e suite reads this cell by it. -->
        <td class="mono wide requested"
          >{#if requested}{requested}{:else if !t.batch_job_id}{@render dash(
              NO_BATCH_JOB,
            )}{:else}{@render dash('unknown')}{/if}</td
        >
        <!-- `host`: the e2e suite reads this cell by it. -->
        <td class="mono wide host"
          >{#if host}{host}{:else if !t.batch_job_id}{@render dash(
              NO_BATCH_JOB,
            )}{:else}{@render dash('unknown')}{/if}</td
        >
        <td class="num wide">{jobCostText(t.cost_usd)}</td>
      </tr>
    {/each}
  </tbody>
</table>
<p class="muted footer">
  <span class="keys">j/k to move · Esc to close ·</span>
  {rows.length < total ? `Showing ${rows.length} of ${total}` : `${total} jobs`}
  {#if rows.length < total}
    · <button class="linkish" onclick={onshowmore} disabled={loadingMore}
      >{loadingMore ? 'Loading…' : `Show ${Math.min(page, total - rows.length)} more`}</button
    >
  {/if}
</p>

<style>
  /* With the job's pane open, the table drops its widest columns. */
  :global(.with-pane) .wide {
    display: none;
  }
  @media (max-width: 800px) {
    .wide,
    .narrow-hide {
      display: none;
    }
    .task-key {
      word-break: normal;
      overflow-wrap: anywhere;
    }
  }
  tbody tr {
    cursor: pointer;
  }
  tr.selected td {
    background: color-mix(in srgb, var(--accent, #4a7cf5) 10%, transparent);
  }
  .task-key {
    font-family: var(--mono);
    font-size: 0.8rem;
    word-break: break-all;
  }
  .key {
    font-family: var(--mono);
    font-size: 0.8rem;
    text-align: left;
    color: inherit;
  }
  .key:hover {
    color: var(--accent);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  td.num {
    color: var(--text-muted);
  }
  .exit.nonzero {
    color: var(--fg-fail);
    font-weight: 600;
  }
  .mono {
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .cached {
    margin-left: 0.35rem;
    font-size: 0.66rem;
    color: var(--text-muted);
  }
  .termination {
    margin-left: 0.35rem;
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--fg-fail);
  }
  .footer {
    font-size: 0.78rem;
    margin-top: 0.5rem;
  }
  @media (max-width: 800px) {
    .keys {
      display: none;
    }
  }
  .linkish {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
    font: inherit;
  }
</style>
