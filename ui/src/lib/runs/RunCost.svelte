<script lang="ts">
  import type { RunDetail } from '$lib/api';
  import { fmtUsd } from '$lib/util/format';
  import { costBasis } from '$lib/runs/runFacts';
  import { lifecycleLabel } from '$lib/runs/attempts';

  // The run's estimated cost: totals and a breakdown by instance type.
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();
</script>

<section class="cost">
  <h2>Cost <span class="hint">estimated</span></h2>
  {#if run.cost && run.cost.priced_jobs > 0}
    <dl class="cost-totals">
      <dt>All attempts</dt>
      <dd>{fmtUsd(run.cost.estimated_usd)}</dd>
      <dt title="Each task's final attempt plus the coordinator: the run without retries">
        Without retries
      </dt>
      <dd>{fmtUsd(run.cost.winning_usd)}</dd>
      <dt>Coordinator</dt>
      <dd>{fmtUsd(run.cost.coordinator_usd)}</dd>
      <dt>Compute / root volume</dt>
      <dd>{fmtUsd(run.cost.compute_usd)} / {fmtUsd(run.cost.ebs_usd)}</dd>
    </dl>
    <table class="cost-by-type">
      <thead>
        <tr><th>Instance type</th><th>Capacity</th><th>Jobs</th><th>Hours</th><th>Cost</th></tr>
      </thead>
      <tbody>
        {#each run.cost.by_instance_type as row (`${row.instance_type}|${row.lifecycle}`)}
          <tr>
            <td class="mono">{row.instance_type}</td>
            <td>{lifecycleLabel(row.lifecycle) ?? '--'}</td>
            <td>{row.jobs}</td>
            <td>{row.hours.toFixed(2)}</td>
            <td>{fmtUsd(row.usd)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="hint">{costBasis(run.cost)}</p>
  {:else}
    <p class="muted">No jobs have been priced yet.</p>
  {/if}
</section>

<style>
  .cost {
    margin-bottom: 1.25rem;
  }
  .cost-totals {
    display: grid;
    grid-template-columns: max-content max-content;
    gap: 0.2rem 1.2rem;
    font-size: 0.82rem;
    margin: 0.6rem 0;
  }
  .cost-totals dt {
    color: var(--text-muted);
  }
  .cost-totals dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
  .cost-by-type {
    width: auto;
    font-size: 0.8rem;
  }
  .cost-by-type td,
  .cost-by-type th {
    padding: 0.25rem 1rem 0.25rem 0;
    text-align: left;
    font-variant-numeric: tabular-nums;
  }
  /* Inline after a label or a heading, so a step smaller than the shared hint. */
  .hint {
    font-size: 0.72rem;
    font-weight: normal;
    margin-left: 0.3rem;
  }
  .mono {
    font-size: 0.78rem;
    word-break: break-all;
  }
</style>
