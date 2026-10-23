<script lang="ts">
  import type { QcGeneralStats } from '#lib/api/index.js';
  import { jobsHref as jobsViewHref } from '#lib/runs/jobsUrl.js';
  import { formatQcValue, qcStatus, sortSamples } from './qcStats';
  import { plural } from '#lib/util/format.js';
  import { ariaSort, sortArrow, type SortDir } from '#lib/util/sort.js';

  // MultiQC's General Statistics as a native table: sortable, MultiQC's pass/warn/fail rules shown
  // as colored cells with a symbol, each sample linked to its jobs. Columns MultiQC hides by default
  // stay hidden until asked for.
  interface Props {
    runId: string;
    stats: QcGeneralStats;
  }
  let { runId, stats }: Props = $props();

  let showAll = $state(false);
  let sortBy = $state<string | null>(null);
  let descending = $state(false);

  const hiddenCount = $derived(stats.columns.filter((c) => c.hidden).length);
  const columns = $derived(showAll ? stats.columns : stats.columns.filter((c) => !c.hidden));
  // Sorted by a column that is hidden again: back to sample order, as its header then says.
  $effect(() => {
    if (sortBy !== null && !columns.some((c) => c.id === sortBy)) {
      sortBy = null;
      descending = false;
    }
  });
  const dir = $derived<SortDir>(descending ? 'desc' : 'asc');
  const rows = $derived(sortSamples(stats.samples, sortBy, descending));

  // `null` is the Sample column.
  function sort(id: string | null) {
    if (sortBy === id) descending = !descending;
    else {
      sortBy = id;
      descending = false;
    }
  }
  const arrow = (id: string | null) => sortArrow(id, sortBy, dir);
  const headerSort = (id: string | null) => ariaSort(id, sortBy, dir);
  const SYMBOL = { pass: '✓', warn: '!', fail: '✗' } as const;
  // The Jobs view searched for the sample; a sample name is in the fragment, never a query string.
  const jobsHref = (sample: string) => jobsViewHref(runId, { q: sample });
</script>

<details class="qc" open>
  <summary>
    General statistics
    <span class="muted">· {plural(stats.samples.length, 'sample')}</span>
  </summary>
  {#if hiddenCount > 0}
    <label class="toggle">
      <input type="checkbox" bind:checked={showAll} /> Show all columns ({hiddenCount} hidden by MultiQC)
    </label>
  {/if}
  <div class="scroll">
    <table>
      <thead>
        <tr>
          <th aria-sort={headerSort(null)}>
            <button type="button" onclick={() => sort(null)}>Sample{arrow(null)}</button>
          </th>
          {#each columns as c (c.id)}
            <th aria-sort={headerSort(c.id)} class="num">
              <button
                type="button"
                title={[c.namespace, c.description].filter(Boolean).join(': ')}
                onclick={() => sort(c.id)}>{c.title}{arrow(c.id)}</button
              >
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each rows as s (s.name)}
          <tr>
            <th scope="row" class="sample">
              <a href={jobsHref(s.name)} title="This sample's jobs">{s.name}</a>
            </th>
            {#each columns as c (c.id)}
              {@const status = qcStatus(s.values[c.id], c.cond_formatting_rules)}
              <td class={['num', status]}>
                {formatQcValue(s.values[c.id], c)}{#if status}<span class="mark" aria-hidden="true"
                    >{SYMBOL[status]}</span
                  ><span class="sr-only"> ({status})</span>{/if}
              </td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</details>

<style>
  .qc {
    margin: 0 0 1rem;
  }
  summary {
    cursor: pointer;
    font-weight: 600;
    margin-bottom: 0.5rem;
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    margin-bottom: 0.4rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .scroll {
    overflow: auto;
    max-height: 50vh;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: var(--bg-content);
  }
  table {
    width: auto;
    min-width: 100%;
    margin: 0;
    border: none;
    border-radius: 0;
    overflow: visible;
    border-collapse: separate;
    border-spacing: 0;
    font-size: 0.78rem;
  }
  thead th {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 0;
    background: var(--th-bg);
    text-transform: none;
    letter-spacing: normal;
    white-space: nowrap;
  }
  thead th button {
    width: 100%;
    padding: 0.35rem 0.6rem;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    font-weight: 600;
    text-align: inherit;
    cursor: pointer;
  }
  th.num,
  td.num {
    text-align: right;
  }
  td,
  th.sample {
    padding: 0.3rem 0.6rem;
    border-top: 1px solid var(--border);
    white-space: nowrap;
  }
  th.sample {
    text-align: left;
    font-weight: 500;
  }
  th.sample a {
    color: var(--accent);
  }
  td.pass {
    color: var(--fg-ok);
  }
  td.warn {
    color: var(--fg-warn);
    font-weight: 600;
  }
  td.fail {
    color: var(--fg-fail);
    font-weight: 600;
  }
  .mark {
    margin-left: 0.25rem;
  }
</style>
