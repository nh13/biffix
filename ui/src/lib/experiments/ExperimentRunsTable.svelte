<script lang="ts">
  // An experiment's runs in the Expert view: a table with run ids (the Lab view lists them as rows).
  import type { RunSummary } from '#lib/api/index.js';
  import { fmtDate } from '#lib/util/format.js';
  import StatusPill from '#lib/runs/StatusPill.svelte';
  import { pipelineTitle, runName } from '#lib/runs/runName.js';
  import { runTabHref } from '#lib/runs/runNav.js';

  let { runs }: { runs: RunSummary[] } = $props();
</script>

<table>
  <thead>
    <tr>
      <th scope="col">Run</th>
      <th scope="col">Workflow</th>
      <th scope="col">Status</th>
      <th scope="col">Submitted</th>
    </tr>
  </thead>
  <tbody>
    {#each runs as r (r.run_id)}
      <tr>
        <td>
          <a href={runTabHref(r.run_id, 'overview')}>{runName(r)}</a>
          <div class="sub mono">{r.run_id}</div>
        </td>
        <td>{pipelineTitle(r) ?? r.workflow ?? '--'}</td>
        <td><StatusPill state={r.state} /></td>
        <td>{fmtDate(r.submitted_at)}</td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .sub {
    margin: 0;
    font-size: 0.72rem;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
</style>
