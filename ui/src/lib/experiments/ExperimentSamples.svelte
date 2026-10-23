<script lang="ts">
  // Every sample × analysis of an experiment, each cell the sample's state in that analysis: the
  // first dozen rows, all on asking.
  import SampleStateChip from '$lib/runs/analysis/SampleStateChip.svelte';
  import type { SampleColumn, SampleRow } from './experiments';

  let { table }: { table: { columns: SampleColumn[]; rows: SampleRow[] } } = $props();

  const SAMPLE_ROWS = 12;
  let allSamples = $state(false);
  const shownRows = $derived(allSamples ? table.rows : table.rows.slice(0, SAMPLE_ROWS));
</script>

<div class="scroll">
  <table class="samples" aria-label="Samples by analysis">
    <thead>
      <tr>
        <th scope="col">Sample</th>
        {#each table.columns as c (c.runId)}
          <th scope="col" title={c.runId}>{c.label}</th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each shownRows as row (row.id)}
        <tr>
          <th scope="row">{row.id}</th>
          {#each row.cells as cell, i (table.columns[i].runId)}
            <td><SampleStateChip value={cell} /></td>
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
</div>
{#if table.rows.length > SAMPLE_ROWS}
  <p class="muted small">
    {allSamples
      ? `Showing all ${table.rows.length}`
      : `Showing ${SAMPLE_ROWS} of ${table.rows.length}`} ·
    <button class="link" onclick={() => (allSamples = !allSamples)}
      >{allSamples ? 'Show fewer' : 'Show all'}</button
    >
  </p>
{/if}

<style>
  .scroll {
    overflow-x: auto;
  }
  .samples {
    font-size: 0.82rem;
    margin-top: 0;
  }
  .samples th,
  .samples td {
    white-space: nowrap;
    padding: 0.35rem 0.6rem;
  }
  .samples tbody th {
    font-weight: 600;
    text-transform: none;
    letter-spacing: normal;
    font-size: 0.82rem;
    background: none;
    color: var(--text);
  }
  .small {
    font-size: 0.8rem;
    margin: 0.4rem 0 0;
  }
</style>
